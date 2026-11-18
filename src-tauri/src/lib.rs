mod copy;
mod device;
mod discover;

use copy::{BackupRequest, Measure, Preview, VerifyResult};
use discover::Item;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

struct AppState {
    items: Mutex<Vec<Item>>,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    started_ms: Arc<AtomicU64>,
}

#[derive(Serialize)]
struct Drive {
    name: String,
    mount: String,
    fs: String,
    total: u64,
    available: u64,
    removable: bool,
    system: bool,
}

fn is_system_mount(mount: &str) -> bool {
    if cfg!(windows) {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        mount.to_uppercase().starts_with(&sys.to_uppercase())
    } else {
        mount == "/" || mount.starts_with("/System") || mount.starts_with("/private") || mount.starts_with("/boot")
    }
}

fn is_fat(fs: &str) -> bool {
    matches!(fs.to_lowercase().as_str(), "vfat" | "msdos" | "fat" | "fat16" | "fat32")
}

/// Free space and file system of the disk that holds `dir` (longest matching mount point).
fn disk_for(dir: &str) -> Option<(u64, String)> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .iter()
        .filter(|d| Path::new(dir).starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.available_space(), d.file_system().to_string_lossy().to_string()))
}

#[tauri::command]
fn list_drives() -> Vec<Drive> {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else { return Vec::new() };
    let mut out: Vec<Drive> = disks
        .iter()
        .map(|d| {
            let mount = d.mount_point().to_string_lossy().to_string();
            Drive {
                name: d.name().to_string_lossy().to_string(),
                system: is_system_mount(&mount),
                mount,
                fs: d.file_system().to_string_lossy().to_string(),
                total: d.total_space(),
                available: d.available_space(),
                removable: d.is_removable(),
            }
        })
        .filter(|d| d.total > 0)
        .collect();
    out.sort_by(|a, b| (a.system, !a.removable, &a.mount).cmp(&(b.system, !b.removable, &b.mount)));
    out.dedup_by(|a, b| a.mount == b.mount);
    out
}

#[tauri::command]
fn discover(state: State<AppState>) -> Vec<Item> {
    let items = discover::discover();
    *state.items.lock().unwrap() = items.clone();
    items
}

#[tauri::command]
async fn measure(state: State<'_, AppState>, id: String, skip_build: bool) -> Result<Measure, String> {
    let item = state
        .items
        .lock()
        .unwrap()
        .iter()
        .find(|i| i.id == id)
        .cloned()
        .ok_or("unknown item")?;
    tauri::async_runtime::spawn_blocking(move || copy::measure(&item, skip_build))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn device_info() -> device::Device {
    device::current()
}

/// This device's backup folder on the destination, if it already has one.
#[tauri::command]
fn existing_backup(dest_dir: String) -> Option<String> {
    let root = copy::backup_root(&dest_dir, &device::current(), "");
    root.is_dir().then(|| root.display().to_string())
}

/// All devices that have a backup folder on the destination.
#[tauri::command]
fn list_devices(dest_dir: String) -> Vec<copy::DeviceEntry> {
    copy::list_devices(&dest_dir, &device::current())
}

/// Older unlabelled `Backup-*` folders that this device could claim.
#[tauri::command]
fn legacy_backups(dest_dir: String) -> Vec<String> {
    copy::legacy_backups(&dest_dir)
}

#[tauri::command]
fn adopt_legacy(dest_dir: String, legacy: String, label: Option<String>) -> Result<String, String> {
    copy::adopt_legacy(&dest_dir, &legacy, &device::current(), label.as_deref().unwrap_or(""))
}

/// What a backup of one item would copy right now, compared with what is already on the destination.
#[tauri::command]
async fn preview_item(state: State<'_, AppState>, id: String, dest_dir: String, skip_build: bool) -> Result<Preview, String> {
    let item = state.items.lock().unwrap().iter().find(|i| i.id == id).cloned().ok_or("unknown item")?;
    let fat = disk_for(&dest_dir).is_some_and(|(_, fs)| is_fat(&fs));
    let root = copy::backup_root(&dest_dir, &device::current(), "");
    tauri::async_runtime::spawn_blocking(move || copy::preview(&item, skip_build, &root, fat))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn start_backup(app: AppHandle, state: State<AppState>, req: BackupRequest) -> Result<(), String> {
    if !Path::new(&req.dest_dir).is_dir() {
        return Err("The destination folder does not exist.".into());
    }
    let items: Vec<Item> = {
        let all = state.items.lock().unwrap();
        req.item_ids.iter().filter_map(|id| all.iter().find(|i| &i.id == id).cloned()).collect()
    };
    if items.is_empty() {
        return Err("Nothing is selected.".into());
    }
    if items.iter().any(|i| i.sensitive) && req.passphrase.as_deref().map_or(true, |p| p.chars().count() < 12) {
        return Err("Choose a passphrase of at least 12 characters to encrypt the secrets.".into());
    }
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("A backup is already running.".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    if let Ok(mut last) = copy::LAST_PROGRESS.lock() {
        *last = None;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    state.started_ms.store(now_ms, Ordering::SeqCst);
    let (available, fs) = disk_for(&req.dest_dir).unwrap_or((u64::MAX, String::new()));
    let cancel = state.cancel.clone();
    let running = state.running.clone();
    std::thread::spawn(move || {
        let summary = copy::run(&app, items, &req, &device::current(), is_fat(&fs), available, &cancel);
        running.store(false, Ordering::SeqCst);
        if let Ok(mut last) = copy::LAST_PROGRESS.lock() {
            *last = None;
        }
        let _ = app.emit("backup-done", summary);
    });
    Ok(())
}

#[derive(Serialize)]
struct BackupStatus {
    running: bool,
    started_ms: u64,
    progress: Option<copy::ProgressEvent>,
}

/// Lets the window find out whether a backup is already running (for example after a reload).
#[tauri::command]
fn backup_status(state: State<AppState>) -> BackupStatus {
    BackupStatus {
        running: state.running.load(Ordering::SeqCst),
        started_ms: state.started_ms.load(Ordering::SeqCst),
        progress: copy::LAST_PROGRESS.lock().ok().and_then(|p| p.clone()),
    }
}

#[tauri::command]
fn cancel_backup(state: State<AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn verify_backup(state: State<'_, AppState>, dest: String) -> Result<VerifyResult, String> {
    state.cancel.store(false, Ordering::SeqCst);
    let cancel = state.cancel.clone();
    tauri::async_runtime::spawn_blocking(move || copy::verify(&dest, &cancel))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            items: Mutex::new(Vec::new()),
            cancel: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            started_ms: Arc::new(AtomicU64::new(0)),
        })
        .invoke_handler(tauri::generate_handler![
            list_drives,
            discover,
            measure,
            device_info,
            existing_backup,
            list_devices,
            legacy_backups,
            adopt_legacy,
            preview_item,
            start_backup,
            cancel_backup,
            backup_status,
            verify_backup
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

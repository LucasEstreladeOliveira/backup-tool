//! Walking, copying, hashing and (for secrets) encrypting.

use crate::device::{Device, DeviceMarker};
use crate::discover::Item;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
use tauri::{AppHandle, Emitter};
use walkdir::{DirEntry, WalkDir};

const BUILD_DIRS: &[&str] = &[
    "node_modules", ".next", ".nuxt", ".venv", "venv", "__pycache__", ".gradle", "Pods",
    ".turbo", ".parcel-cache",
];
const CACHE_DIRS: &[&str] = &[
    "Cache", "Code Cache", "GPUCache", "CachedData", "CachedExtensions", "CachedExtensionVSIXs",
    "Crashpad", "logs", "Service Worker", "blob_storage", "DawnCache", "GrShaderCache", "ShaderCache",
];
const JUNK_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
const FAT_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
const MAX_LISTED: usize = 500;

#[derive(Clone)]
pub struct Filter {
    skip_build: bool,
    config: bool,
    exclude_names: Vec<String>,
}

impl Filter {
    pub fn for_item(item: &Item, skip_build: bool) -> Self {
        let content = matches!(item.category.as_str(), "files" | "projects");
        Filter {
            skip_build: skip_build && content,
            config: !content,
            exclude_names: item.exclude_names.clone(),
        }
    }

    fn skip(&self, e: &DirEntry) -> bool {
        if e.depth() == 0 {
            return false;
        }
        let name = e.file_name().to_string_lossy();
        if self.exclude_names.iter().any(|n| n.as_str() == &*name) {
            return true;
        }
        if e.file_type().is_dir() {
            if self.skip_build {
                if BUILD_DIRS.contains(&&*name) {
                    return true;
                }
                if &*name == "target"
                    && e.path().parent().map_or(false, |p| p.join("Cargo.toml").exists())
                {
                    return true;
                }
            }
            self.config && CACHE_DIRS.contains(&&*name)
        } else {
            JUNK_FILES.contains(&&*name)
        }
    }
}

/// Calls `f(path, rel, size, mtime)` for every regular file of the item. `rel` starts with
/// the item's `dest_name`. Problems (symlinks, unreadable entries) are pushed to `notes`.
fn walk(item: &Item, filter: &Filter, notes: &mut Vec<String>, mut f: impl FnMut(&Path, PathBuf, u64, Option<SystemTime>)) {
    if item.kind == "file" {
        if let Ok(m) = fs::metadata(&item.path) {
            f(&item.path, PathBuf::from(&item.dest_name), m.len(), m.modified().ok());
        }
        return;
    }
    let it = WalkDir::new(&item.path).follow_links(false).into_iter().filter_entry(|e| !filter.skip(e));
    for entry in it {
        match entry {
            Ok(e) if e.file_type().is_file() => {
                if let Ok(m) = e.metadata() {
                    let rel = Path::new(&item.dest_name).join(e.path().strip_prefix(&item.path).unwrap_or(e.path()));
                    f(e.path(), rel, m.len(), m.modified().ok());
                }
            }
            Ok(e) if e.file_type().is_symlink() => notes.push(format!("Skipped symlink: {}", e.path().display())),
            Ok(_) => {}
            Err(err) => notes.push(format!("Unreadable: {err}")),
        }
    }
}

#[derive(Serialize)]
pub struct Measure {
    pub bytes: u64,
    pub files: u64,
}

pub fn measure(item: &Item, skip_build: bool) -> Measure {
    let filter = Filter::for_item(item, skip_build);
    let mut m = Measure { bytes: 0, files: 0 };
    walk(item, &filter, &mut Vec::new(), |_, _, size, _| {
        m.bytes += size;
        m.files += 1;
    });
    m
}

/// Makes one path component safe for FAT/exFAT/NTFS targets. Returns the component and
/// whether it had to be changed.
pub fn sanitize(name: &str) -> (String, bool) {
    let mut s: String = name
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/') || (c as u32) < 32 { '_' } else { c })
        .collect();
    while s.ends_with(' ') || s.ends_with('.') {
        s.pop();
    }
    if s.is_empty() {
        s.push('_');
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    let changed = s != name;
    (s, changed)
}

fn safe_join(base: &Path, rel: &Path, renamed: &mut Vec<String>) -> PathBuf {
    let mut out = base.to_path_buf();
    for comp in rel.components() {
        let raw = comp.as_os_str().to_string_lossy();
        let (clean, changed) = sanitize(&raw);
        if changed {
            renamed.push(format!("Renamed for compatibility: {raw} -> {clean}"));
        }
        out.push(clean);
    }
    out
}

fn category_dir(cat: &str) -> &'static str {
    match cat {
        "files" => "Files",
        "projects" => "Projects",
        "shell" => "Terminal",
        "ide" => "IDE",
        "devtools" => "DevTools",
        "browser" => "Browser",
        _ => "Other",
    }
}

#[derive(Deserialize, Clone)]
pub struct BackupRequest {
    pub dest_dir: String,
    pub item_ids: Vec<String>,
    pub skip_build: bool,
    pub passphrase: Option<String>,
    pub export_package_list: bool,
    /// Optional friendlier name for this device (defaults to the computer name).
    pub device_label: Option<String>,
}

/// Latest progress of the running backup, so a (re)loaded window can pick it up.
pub static LAST_PROGRESS: Mutex<Option<ProgressEvent>> = Mutex::new(None);

#[derive(Serialize, Clone)]
pub struct ProgressEvent {
    phase: &'static str,
    bytes_done: u64,
    bytes_total: u64,
    files_done: u64,
    files_total: u64,
    current: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct ManifestEntry {
    source: String,
    dest: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize, Default, Clone)]
pub struct Summary {
    pub dest: String,
    /// Files now present in the backup (new + updated + unchanged).
    pub files: u64,
    /// Bytes written during this run.
    pub bytes: u64,
    pub new_files: u64,
    pub updated_files: u64,
    pub unchanged_files: u64,
    pub secrets_file: Option<String>,
    pub cancelled: bool,
    pub fatal: Option<String>,
    pub skipped: Vec<String>,
    pub skipped_total: usize,
    pub errors: Vec<String>,
    pub errors_total: usize,
    pub seconds: u64,
}

struct Progress<'a> {
    app: &'a AppHandle,
    ev: ProgressEvent,
    last: Instant,
}

impl Progress<'_> {
    fn emit(&mut self, force: bool) {
        if force || self.last.elapsed().as_millis() >= 120 {
            self.last = Instant::now();
            if let Ok(mut last) = LAST_PROGRESS.lock() {
                *last = Some(self.ev.clone());
            }
            let _ = self.app.emit("backup-progress", self.ev.clone());
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FileState {
    New,
    Updated,
    Unchanged,
}

struct PlannedFile {
    src: PathBuf,
    dest: PathBuf,
    size: u64,
    mtime: Option<SystemTime>,
    state: FileState,
    /// Hash reused from the previous manifest for unchanged files.
    known_sha: Option<String>,
}

/// Compares a source file with its backed-up copy by size and modified time
/// (2 s tolerance, because FAT stores times with 2 s resolution).
fn classify(dest: &Path, size: u64, mtime: Option<SystemTime>) -> FileState {
    let Ok(dm) = fs::metadata(dest) else { return FileState::New };
    if dm.len() != size {
        return FileState::Updated;
    }
    match (dm.modified().ok(), mtime) {
        (Some(d), Some(s)) if d.duration_since(s).unwrap_or_else(|e| e.duration()).as_secs() <= 2 => FileState::Unchanged,
        _ => FileState::Updated,
    }
}

const MARKER: &str = ".backup-device.json";
const LABEL_MAX: usize = 40;

fn read_marker(folder: &Path) -> Option<DeviceMarker> {
    serde_json::from_str(&fs::read_to_string(folder.join(MARKER)).ok()?).ok()
}

/// This device's folder on the drive: `<dest>/Backup/<name>-<key>`. An existing folder is
/// found by the key inside its marker file, so renaming the folder does not lose it.
pub fn backup_root(dest_dir: &str, dev: &Device, label: &str) -> PathBuf {
    let parent = Path::new(dest_dir).join("Backup");
    let existing = fs::read_dir(&parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| read_marker(p).is_some_and(|m| m.device_key == dev.key));
    existing.unwrap_or_else(|| {
        let name = if label.trim().is_empty() { &dev.name } else { label.trim() };
        let clean: String = sanitize(name).0.chars().take(LABEL_MAX).collect();
        parent.join(format!("{clean}-{}", dev.key))
    })
}

pub fn write_marker(root: &Path, dev: &Device, label: &str) {
    let name = if label.trim().is_empty() { dev.name.clone() } else { label.trim().to_string() };
    let marker = DeviceMarker {
        device_key: dev.key.clone(),
        device_name: name,
        os: dev.os.clone(),
        last_backup: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    };
    if fs::create_dir_all(root).is_ok() {
        if let Ok(json) = serde_json::to_string_pretty(&marker) {
            let _ = fs::write(root.join(MARKER), json);
        }
    }
}

#[derive(Serialize)]
pub struct DeviceEntry {
    pub folder: String,
    pub name: String,
    pub os: String,
    pub last_backup: String,
    pub this_device: bool,
}

/// Every device that has a backup folder on this drive.
pub fn list_devices(dest_dir: &str, dev: &Device) -> Vec<DeviceEntry> {
    let mut out: Vec<DeviceEntry> = fs::read_dir(Path::new(dest_dir).join("Backup"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = read_marker(&e.path())?;
            Some(DeviceEntry {
                folder: e.path().display().to_string(),
                this_device: m.device_key == dev.key,
                name: m.device_name,
                os: m.os,
                last_backup: m.last_backup,
            })
        })
        .collect();
    out.sort_by(|a, b| (!a.this_device, &a.name).cmp(&(!b.this_device, &b.name)));
    out
}

/// Older, unlabelled backups (`Backup-YYYY-MM-DD` folders) that could be claimed by a device.
pub fn legacy_backups(dest_dir: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dest_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Backup-")))
        .map(|p| p.display().to_string())
        .collect();
    v.sort();
    v
}

/// Moves a legacy `Backup-*` folder into this device's folder and labels it.
pub fn adopt_legacy(dest_dir: &str, legacy: &str, dev: &Device, label: &str) -> Result<String, String> {
    let legacy_path = PathBuf::from(legacy);
    let is_legacy = legacy_path.parent() == Some(Path::new(dest_dir))
        && legacy_path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Backup-"))
        && legacy_path.is_dir();
    if !is_legacy {
        return Err("That is not an earlier backup folder on this drive.".into());
    }
    let root = backup_root(dest_dir, dev, label);
    if root.exists() {
        return Err("This device already has a backup folder on this drive.".into());
    }
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::rename(&legacy_path, &root).map_err(|e| format!("Could not move the folder: {e}"))?;
    write_marker(&root, dev, label);
    Ok(root.display().to_string())
}

fn load_manifest(root: &Path) -> BTreeMap<String, ManifestEntry> {
    let entries: Vec<ManifestEntry> = fs::read_to_string(root.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    entries.into_iter().map(|e| (e.dest.clone(), e)).collect()
}

fn plan_item(item: &Item, skip_build: bool, root: &Path, fat_limit: bool, notes: &mut Vec<String>, renamed: &mut Vec<String>) -> Vec<PlannedFile> {
    let filter = Filter::for_item(item, skip_build);
    let base = root.join(category_dir(&item.category));
    let mut found = Vec::new();
    walk(item, &filter, notes, |src, rel, size, mtime| found.push((src.to_path_buf(), rel, size, mtime)));
    let mut plan = Vec::new();
    for (src, rel, size, mtime) in found {
        if src.starts_with(root) {
            continue; // never copy the backup folder into itself
        }
        if fat_limit && size > FAT_MAX_FILE {
            notes.push(format!("Skipped (over FAT32's 4 GB file limit): {}", src.display()));
            continue;
        }
        let dest = safe_join(&base, &rel, renamed);
        let state = classify(&dest, size, mtime);
        plan.push(PlannedFile { dest, src, size, mtime, state, known_sha: None });
    }
    plan
}

#[derive(Serialize, Default)]
pub struct Preview {
    pub new_files: u64,
    pub new_bytes: u64,
    pub updated_files: u64,
    pub updated_bytes: u64,
    pub unchanged_files: u64,
    pub unchanged_bytes: u64,
}

/// What a backup of `item` into `root` would do right now (no hashing, nothing written).
pub fn preview(item: &Item, skip_build: bool, root: &Path, fat_limit: bool) -> Preview {
    let mut p = Preview::default();
    for f in plan_item(item, skip_build, root, fat_limit, &mut Vec::new(), &mut Vec::new()) {
        let (files, bytes) = match f.state {
            FileState::New => (&mut p.new_files, &mut p.new_bytes),
            FileState::Updated => (&mut p.updated_files, &mut p.updated_bytes),
            FileState::Unchanged => (&mut p.unchanged_files, &mut p.unchanged_bytes),
        };
        *files += 1;
        *bytes += f.size;
    }
    p
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_file(path: &Path, cancel: &AtomicBool, prog: Option<&mut Progress>) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut prog = prog;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        if let Some(p) = prog.as_deref_mut() {
            p.ev.bytes_done += n as u64;
            p.emit(false);
        }
    }
    Ok(hex(&h.finalize()))
}

fn copy_one(pf: &PlannedFile, cancel: &AtomicBool, prog: &mut Progress) -> Result<String, String> {
    if let Some(parent) = pf.dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create folder: {e}"))?;
    }
    let mut src = fs::File::open(&pf.src).map_err(|e| format!("open: {e}"))?;
    let mut dst = BufWriter::with_capacity(1 << 20, fs::File::create(&pf.dest).map_err(|e| format!("create: {e}"))?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let result: Result<(), String> = (|| {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let n = src.read(&mut buf).map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            dst.write_all(&buf[..n]).map_err(|e| format!("write: {e}"))?;
            prog.ev.bytes_done += n as u64;
            prog.emit(false);
        }
        dst.flush().map_err(|e| format!("write: {e}"))
    })();
    drop(dst);
    if let Err(e) = result {
        let _ = fs::remove_file(&pf.dest);
        return Err(e);
    }
    if let Some(mt) = pf.mtime {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&pf.dest) {
            let _ = f.set_times(fs::FileTimes::new().set_modified(mt));
        }
    }
    Ok(hex(&h.finalize()))
}

fn write_secrets(items: &[&Item], skip_build: bool, out: &Path, pass: &str, notes: &mut Vec<String>) -> Result<(), String> {
    if let Some(p) = out.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let file = fs::File::create(out).map_err(|e| e.to_string())?;
    let enc = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(pass.to_string()));
    let w = enc.wrap_output(BufWriter::new(file)).map_err(|e| e.to_string())?;
    let mut tb = tar::Builder::new(w);
    for item in items {
        let filter = Filter::for_item(item, skip_build);
        let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
        walk(item, &filter, notes, |p, rel, _, _| found.push((p.to_path_buf(), rel)));
        for (p, rel) in found {
            if let Err(e) = tb.append_path_with_name(&p, &rel) {
                notes.push(format!("Secrets: could not add {}: {e}", p.display()));
            }
        }
    }
    let w = tb.into_inner().map_err(|e| e.to_string())?;
    let mut inner = w.finish().map_err(|e| e.to_string())?;
    inner.flush().map_err(|e| e.to_string())
}

fn export_package_list(root: &Path, notes: &mut Vec<String>) {
    use std::process::Command;
    let dir = root.join("Software");
    let _ = fs::create_dir_all(&dir);
    let status = if cfg!(windows) {
        Command::new("winget")
            .args(["export", "-o"])
            .arg(dir.join("winget-packages.json"))
            .args(["--accept-source-agreements", "--disable-interactivity"])
            .status()
    } else if cfg!(target_os = "macos") {
        let brew = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"].into_iter().find(|p| Path::new(p).exists()).unwrap_or("brew");
        Command::new(brew).args(["bundle", "dump", "--force", "--file"]).arg(dir.join("Brewfile")).status()
    } else {
        return;
    };
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => notes.push(format!("Package list export exited with {s}")),
        Err(e) => notes.push(format!("Package list export unavailable: {e}")),
    }
}

fn cap(mut v: Vec<String>) -> (Vec<String>, usize) {
    let total = v.len();
    v.truncate(MAX_LISTED);
    (v, total)
}

pub fn run(app: &AppHandle, items: Vec<Item>, req: &BackupRequest, dev: &Device, fat_limit: bool, available: u64, cancel: &AtomicBool) -> Summary {
    let started = Instant::now();
    let mut summary = Summary::default();
    let mut notes: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut prog = Progress {
        app,
        ev: ProgressEvent { phase: "scanning", bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0, current: String::new() },
        last: Instant::now(),
    };
    prog.emit(true);

    let label = req.device_label.as_deref().unwrap_or("");
    let root = backup_root(&req.dest_dir, dev, label);
    summary.dest = root.display().to_string();
    if let Err(e) = fs::create_dir_all(&root) {
        summary.fatal = Some(format!("Cannot create the backup folder: {e}"));
        return summary;
    }

    // Plan: list every file first so progress and the free-space check are accurate.
    let (secret_items, plain_items): (Vec<&Item>, Vec<&Item>) = items.iter().partition(|i| i.sensitive);
    let previous = load_manifest(&root);
    let mut plan: Vec<PlannedFile> = Vec::new();
    let mut renamed: Vec<String> = Vec::new();
    for item in &plain_items {
        plan.extend(plan_item(item, req.skip_build, &root, fat_limit, &mut notes, &mut renamed));
    }
    notes.extend(renamed);
    let rel_dest = |pf: &PlannedFile| pf.dest.strip_prefix(&root).unwrap_or(&pf.dest).to_string_lossy().replace('\\', "/");
    // Unchanged files only need re-reading when the previous manifest has no hash for them.
    for pf in plan.iter_mut().filter(|p| p.state == FileState::Unchanged) {
        pf.known_sha = previous.get(&rel_dest(pf)).filter(|e| e.size == pf.size && !e.sha256.is_empty()).map(|e| e.sha256.clone());
    }
    let to_write: u64 = plan.iter().filter(|p| p.state != FileState::Unchanged).map(|p| p.size).sum();
    let to_read: u64 = plan.iter().filter(|p| p.state == FileState::Unchanged && p.known_sha.is_none()).map(|p| p.size).sum();
    prog.ev.bytes_total = to_write + to_read;
    prog.ev.files_total = plan.len() as u64;
    if to_write > available {
        summary.fatal = Some(format!(
            "Not enough free space: need {:.1} GB, drive has {:.1} GB free.",
            to_write as f64 / 1e9,
            available as f64 / 1e9
        ));
        return summary;
    }

    // Copy.
    prog.ev.phase = "copying";
    prog.emit(true);
    let mut manifest = previous.clone();
    for pf in &plan {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        prog.ev.current = pf.src.display().to_string();
        prog.emit(false);
        let outcome = match (pf.state, &pf.known_sha) {
            (FileState::Unchanged, Some(sha)) => Ok(sha.clone()),
            (FileState::Unchanged, None) => hash_file(&pf.dest, cancel, Some(&mut prog)),
            _ => copy_one(pf, cancel, &mut prog),
        };
        match outcome {
            Ok(sha) => {
                summary.files += 1;
                match pf.state {
                    FileState::New => summary.new_files += 1,
                    FileState::Updated => summary.updated_files += 1,
                    FileState::Unchanged => summary.unchanged_files += 1,
                }
                if pf.state != FileState::Unchanged {
                    summary.bytes += pf.size;
                }
                let dest = rel_dest(pf);
                manifest.insert(dest.clone(), ManifestEntry { source: pf.src.display().to_string(), dest, size: pf.size, sha256: sha });
            }
            Err(e) if e == "cancelled" => {
                summary.cancelled = true;
                break;
            }
            Err(e) => errors.push(format!("{}: {e}", pf.src.display())),
        }
        prog.ev.files_done += 1;
    }

    // Encrypted secrets archive.
    if !summary.cancelled && !secret_items.is_empty() {
        prog.ev.phase = "encrypting";
        prog.ev.current = "Encrypting secrets".into();
        prog.emit(true);
        let out = root.join("Secrets").join("secrets.tar.age");
        match write_secrets(&secret_items, req.skip_build, &out, req.passphrase.as_deref().unwrap_or(""), &mut notes) {
            Ok(()) => summary.secrets_file = Some(out.display().to_string()),
            Err(e) => errors.push(format!("Secrets archive failed: {e}")),
        }
    }

    if !summary.cancelled && req.export_package_list {
        prog.ev.current = "Exporting installed-software list".into();
        prog.emit(true);
        export_package_list(&root, &mut notes);
    }

    write_marker(&root, dev, label);

    // Manifest + log.
    if let Ok(f) = fs::File::create(root.join("manifest.json")) {
        let _ = serde_json::to_writer_pretty(BufWriter::new(f), &manifest.values().collect::<Vec<_>>());
    }
    let log: Vec<String> = notes.iter().chain(errors.iter()).cloned().collect();
    let _ = fs::write(root.join("backup-log.txt"), log.join("\n"));

    summary.seconds = started.elapsed().as_secs();
    (summary.skipped, summary.skipped_total) = cap(notes);
    (summary.errors, summary.errors_total) = cap(errors);
    prog.ev.phase = "done";
    prog.emit(true);
    summary
}

#[derive(Serialize)]
pub struct VerifyResult {
    pub checked: u64,
    pub bad: Vec<String>,
}

/// Re-reads every file listed in `manifest.json` and compares SHA-256 hashes.
pub fn verify(dest: &str, cancel: &AtomicBool) -> Result<VerifyResult, String> {
    let root = Path::new(dest);
    let raw = fs::read_to_string(root.join("manifest.json")).map_err(|e| format!("Cannot read manifest.json: {e}"))?;
    #[derive(Deserialize)]
    struct E {
        dest: String,
        sha256: String,
    }
    let entries: Vec<E> = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let mut res = VerifyResult { checked: 0, bad: Vec::new() };
    for e in entries {
        let rel = Path::new(&e.dest);
        if rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            res.bad.push(format!("Refused unsafe path in manifest: {}", e.dest));
            res.checked += 1;
            continue;
        }
        match hash_file(&root.join(rel), cancel, None) {
            Ok(h) if h == e.sha256 => {}
            Ok(_) => res.bad.push(format!("Hash mismatch: {}", e.dest)),
            Err(err) => res.bad.push(format!("{}: {err}", e.dest)),
        }
        res.checked += 1;
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("backup-tool-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_with_mtime(path: &Path, data: &[u8], mtime: SystemTime) {
        fs::write(path, data).unwrap();
        fs::OpenOptions::new().write(true).open(path).unwrap().set_times(fs::FileTimes::new().set_modified(mtime)).unwrap();
    }

    #[test]
    fn classifies_new_updated_and_unchanged() {
        let d = tmp("classify");
        let t = SystemTime::now() - Duration::from_secs(3600);
        let dest = d.join("f.txt");
        assert!(classify(&dest, 3, Some(t)) == FileState::New);
        write_with_mtime(&dest, b"abc", t + Duration::from_secs(1)); // FAT-style 1 s drift
        assert!(classify(&dest, 3, Some(t)) == FileState::Unchanged);
        assert!(classify(&dest, 4, Some(t)) == FileState::Updated); // size differs
        assert!(classify(&dest, 3, Some(t - Duration::from_secs(60))) == FileState::Updated); // source modified later
    }

    fn dev(key: &str) -> Device {
        Device { key: key.into(), name: "Work PC".into(), os: "windows".into() }
    }

    #[test]
    fn each_device_gets_its_own_folder_found_by_key() {
        let d = tmp("root");
        let base = d.to_str().unwrap();
        let (a, b) = (dev("aaaa1111"), dev("bbbb2222"));
        let root_a = backup_root(base, &a, "");
        assert_eq!(root_a, d.join("Backup").join("Work PC-aaaa1111"));
        write_marker(&root_a, &a, "");
        let root_b = backup_root(base, &b, "Laptop: home");
        assert_eq!(root_b, d.join("Backup").join("Laptop_ home-bbbb2222"));
        write_marker(&root_b, &b, "Laptop: home");
        // Renaming a folder does not lose it: matching is by the key in the marker.
        let renamed = d.join("Backup").join("my old pc");
        fs::rename(&root_a, &renamed).unwrap();
        assert_eq!(backup_root(base, &a, ""), renamed);
        assert_eq!(list_devices(base, &a).len(), 2);
    }

    #[test]
    fn adopts_only_legacy_backup_folders_from_the_drive_root() {
        let d = tmp("adopt");
        let base = d.to_str().unwrap();
        let legacy = d.join("Backup-2026-11-17");
        fs::create_dir_all(legacy.join("Files")).unwrap();
        assert_eq!(legacy_backups(base), vec![legacy.display().to_string()]);
        assert!(adopt_legacy(base, d.join("Content").to_str().unwrap(), &dev("aaaa1111"), "").is_err());
        let root = adopt_legacy(base, legacy.to_str().unwrap(), &dev("aaaa1111"), "").unwrap();
        assert!(Path::new(&root).join("Files").is_dir() && !legacy.exists());
        assert_eq!(backup_root(base, &dev("aaaa1111"), ""), PathBuf::from(root));
    }

    #[test]
    fn preview_counts_only_what_is_missing() {
        let d = tmp("preview");
        let (src, drive) = (d.join("src"), d.join("drive"));
        fs::create_dir_all(src.join("docs")).unwrap();
        fs::create_dir_all(&drive).unwrap();
        let t = SystemTime::now() - Duration::from_secs(3600);
        write_with_mtime(&src.join("docs/a.txt"), b"aaaa", t);
        write_with_mtime(&src.join("docs/b.txt"), b"bb", t);
        let item = Item {
            id: "files-0".into(),
            category: "files".into(),
            label: "docs".into(),
            path: src.join("docs"),
            kind: "dir".into(),
            dest_name: "docs".into(),
            default_selected: true,
            sensitive: false,
            note: None,
            exclude_names: vec![],
        };
        let root = drive.join("Backup");
        let first = preview(&item, true, &root, false);
        assert_eq!((first.new_files, first.unchanged_files), (2, 0));
        // Pretend a.txt was already backed up with the same size and time.
        let backed = root.join("Files/docs");
        fs::create_dir_all(&backed).unwrap();
        write_with_mtime(&backed.join("a.txt"), b"aaaa", t);
        let second = preview(&item, true, &root, false);
        assert_eq!((second.new_files, second.unchanged_files, second.new_bytes), (1, 1, 2));
    }
}

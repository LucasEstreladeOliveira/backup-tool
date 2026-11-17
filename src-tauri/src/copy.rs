//! Walking, copying, hashing and (for secrets) encrypting.

use crate::discover::Item;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
}

#[derive(Serialize, Clone)]
struct ProgressEvent {
    phase: &'static str,
    bytes_done: u64,
    bytes_total: u64,
    files_done: u64,
    files_total: u64,
    current: String,
}

#[derive(Serialize)]
struct ManifestEntry {
    source: String,
    dest: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize, Default, Clone)]
pub struct Summary {
    pub dest: String,
    pub files: u64,
    pub bytes: u64,
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
            let _ = self.app.emit("backup-progress", self.ev.clone());
        }
    }
}

struct PlannedFile {
    src: PathBuf,
    dest: PathBuf,
    size: u64,
    mtime: Option<SystemTime>,
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
    // Already copied (same size, same modified time within FAT's 2s resolution)? Just hash it.
    if let (Ok(dm), Some(sm)) = (fs::metadata(&pf.dest), pf.mtime) {
        if dm.len() == pf.size {
            if let Ok(dt) = dm.modified() {
                let diff = dt.duration_since(sm).or_else(|e| Ok::<_, ()>(e.duration())).unwrap_or_default();
                if diff.as_secs() <= 2 {
                    return hash_file(&pf.dest, cancel, Some(prog));
                }
            }
        }
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

pub fn run(app: &AppHandle, items: Vec<Item>, req: &BackupRequest, fat_limit: bool, available: u64, cancel: &AtomicBool) -> Summary {
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

    let root = Path::new(&req.dest_dir).join(format!("Backup-{}", chrono::Local::now().format("%Y-%m-%d")));
    summary.dest = root.display().to_string();
    if let Err(e) = fs::create_dir_all(&root) {
        summary.fatal = Some(format!("Cannot create the backup folder: {e}"));
        return summary;
    }

    // Plan: list every file first so progress and the free-space check are accurate.
    let (secret_items, plain_items): (Vec<&Item>, Vec<&Item>) = items.iter().partition(|i| i.sensitive);
    let mut plan: Vec<PlannedFile> = Vec::new();
    let mut renamed: Vec<String> = Vec::new();
    for item in &plain_items {
        let filter = Filter::for_item(item, req.skip_build);
        let base = root.join(category_dir(&item.category));
        let mut local = Vec::new();
        walk(item, &filter, &mut notes, |src, rel, size, mtime| local.push((src.to_path_buf(), rel, size, mtime)));
        for (src, rel, size, mtime) in local {
            if src.starts_with(&root) {
                continue; // never copy the backup folder into itself
            }
            if fat_limit && size > FAT_MAX_FILE {
                notes.push(format!("Skipped (over FAT32's 4 GB file limit): {}", src.display()));
                continue;
            }
            plan.push(PlannedFile { dest: safe_join(&base, &rel, &mut renamed), src, size, mtime });
        }
    }
    notes.extend(renamed);
    prog.ev.bytes_total = plan.iter().map(|p| p.size).sum();
    prog.ev.files_total = plan.len() as u64;
    if prog.ev.bytes_total > available {
        summary.fatal = Some(format!(
            "Not enough free space: need {:.1} GB, drive has {:.1} GB free.",
            prog.ev.bytes_total as f64 / 1e9,
            available as f64 / 1e9
        ));
        return summary;
    }

    // Copy.
    prog.ev.phase = "copying";
    prog.emit(true);
    let mut manifest: Vec<ManifestEntry> = Vec::with_capacity(plan.len());
    for pf in &plan {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        prog.ev.current = pf.src.display().to_string();
        prog.emit(false);
        match copy_one(pf, cancel, &mut prog) {
            Ok(sha) => {
                summary.files += 1;
                summary.bytes += pf.size;
                manifest.push(ManifestEntry {
                    source: pf.src.display().to_string(),
                    dest: pf.dest.strip_prefix(&root).unwrap_or(&pf.dest).to_string_lossy().replace('\\', "/"),
                    size: pf.size,
                    sha256: sha,
                });
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

    // Manifest + log.
    if let Ok(f) = fs::File::create(root.join("manifest.json")) {
        let _ = serde_json::to_writer_pretty(BufWriter::new(f), &manifest);
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

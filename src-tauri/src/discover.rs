//! Finds the folders and files on this machine that are worth backing up and
//! groups them into categories. Works on Windows and macOS (and mostly Linux).

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize)]
pub struct Item {
    pub id: String,
    /// files | projects | shell | ide | devtools | browser | secrets
    pub category: String,
    pub label: String,
    pub path: PathBuf,
    /// "dir" or "file"
    pub kind: String,
    /// Name used for this item inside the backup folder.
    pub dest_name: String,
    pub default_selected: bool,
    /// Sensitive items are only ever written inside an encrypted archive.
    pub sensitive: bool,
    pub note: Option<String>,
    /// File or folder names to leave out while copying this item.
    pub exclude_names: Vec<String>,
}

struct Collector {
    items: Vec<Item>,
}

impl Collector {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        category: &str,
        label: &str,
        path: PathBuf,
        dest_name: &str,
        default_selected: bool,
        note: Option<&str>,
        exclude: &[&str],
    ) {
        if !path.exists() || self.items.iter().any(|i| i.path == path) {
            return;
        }
        let kind = if path.is_dir() { "dir" } else { "file" };
        self.items.push(Item {
            id: format!("{}-{}", category, self.items.len()),
            category: category.to_string(),
            label: label.to_string(),
            path,
            kind: kind.to_string(),
            dest_name: dest_name.to_string(),
            default_selected: default_selected && category != "secrets",
            sensitive: category == "secrets",
            note: note.map(str::to_string),
            exclude_names: exclude.iter().map(|s| s.to_string()).collect(),
        });
    }
}

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Package.swift",
    "composer.json",
];

/// Top-level home folders that are never worth listing as "other folders".
const HOME_SKIP: &[&str] = &[
    "library", "appdata", "applications", "public", "3d objects", "links", "saved games",
    "contacts", "favorites", "searches", "application data", "local settings", "cookies",
    "nethood", "printhood", "recent", "sendto", "start menu", "templates", "movies", "music",
    "videos", "pictures", "documents", "desktop", "downloads", "go", "node_modules",
];

fn has_project_marker(dir: &Path, depth: u32) -> bool {
    let Ok(rd) = fs::read_dir(dir) else { return false };
    let mut subdirs = Vec::new();
    for (n, entry) in rd.flatten().enumerate() {
        if n > 300 {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if PROJECT_MARKERS.contains(&name.as_str()) || name.ends_with(".sln") || name.ends_with(".csproj") {
            return true;
        }
        if depth > 0 && !name.starts_with('.') && name != "node_modules" {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                subdirs.push(entry.path());
            }
        }
    }
    subdirs.into_iter().any(|d| has_project_marker(&d, depth - 1))
}

pub fn discover() -> Vec<Item> {
    let mut c = Collector { items: Vec::new() };
    let Some(home) = dirs::home_dir() else { return c.items };
    let config = dirs::config_dir(); // %APPDATA% on Windows, ~/Library/Application Support on macOS
    let local = dirs::data_local_dir(); // %LOCALAPPDATA% on Windows

    // ── Documents & media ────────────────────────────────────────────────
    if let Some(p) = dirs::document_dir() { c.add("files", "Documents", p, "Documents", true, None, &[]); }
    if let Some(p) = dirs::desktop_dir() { c.add("files", "Desktop", p, "Desktop", true, None, &[]); }
    if let Some(p) = dirs::picture_dir() { c.add("files", "Pictures", p, "Pictures", true, None, &[]); }
    if let Some(p) = dirs::download_dir() {
        c.add("files", "Downloads", p, "Downloads", false, Some("Often large and mostly clutter"), &[]);
    }
    if let Some(p) = dirs::video_dir() { c.add("files", "Videos", p, "Videos", false, Some("Usually very large"), &[]); }
    if let Some(p) = dirs::audio_dir() { c.add("files", "Music", p, "Music", false, None, &[]); }

    // ── Other top-level folders in the home directory ────────────────────
    let mut tops: Vec<PathBuf> = fs::read_dir(&home)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    tops.sort();
    for p in tops {
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
        if name.starts_with('.') || HOME_SKIP.contains(&name.to_lowercase().as_str()) {
            continue;
        }
        if has_project_marker(&p, 3) {
            c.add("projects", &name, p, &name, true, None, &[]);
        } else {
            c.add("files", &format!("{name} (other folder)"), p, &name, false, None, &[]);
        }
    }

    // ── Terminal & shell ─────────────────────────────────────────────────
    for f in [
        ".zshrc", ".zshenv", ".zprofile", ".profile", ".bashrc", ".bash_profile", ".p10k.zsh",
        ".wezterm.lua", ".tmux.conf", ".vimrc", ".gitconfig", ".gitignore_global",
    ] {
        c.add("shell", f, home.join(f), f, true, None, &[]);
    }
    c.add("shell", ".zsh", home.join(".zsh"), ".zsh", true, None, &[]);
    c.add("shell", "oh-my-zsh custom", home.join(".oh-my-zsh/custom"), "oh-my-zsh-custom", true, None, &[]);
    c.add(
        "shell",
        ".config",
        home.join(".config"),
        "dot-config",
        true,
        Some("Folders that usually hold CLI tokens (gh, gcloud, op, rclone...) are left out"),
        &["sandboxes", "gh", "hub", "gcloud", "op", "rclone", "configstore", "doctl", "heroku", "netlify", "firebase"],
    );
    // PowerShell profiles and Windows Terminal (Windows only; the paths simply don't exist elsewhere)
    if let Some(d) = dirs::document_dir() {
        c.add("shell", "PowerShell profile", d.join("PowerShell"), "PowerShell", true, None, &[]);
        c.add("shell", "Windows PowerShell profile", d.join("WindowsPowerShell"), "WindowsPowerShell", true, None, &[]);
    }
    if let Some(l) = &local {
        for pkg in ["Microsoft.WindowsTerminal_8wekyb3d8bbwe", "Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe"] {
            c.add(
                "shell",
                &format!("Windows Terminal ({})", if pkg.contains("Preview") { "Preview" } else { "stable" }),
                l.join("Packages").join(pkg).join("LocalState"),
                pkg,
                true,
                None,
                &[],
            );
        }
    }

    // ── IDEs & editors ───────────────────────────────────────────────────
    if let Some(cfg) = &config {
        for (label, dir) in [
            ("VS Code settings", "Code/User"),
            ("VS Code Insiders settings", "Code - Insiders/User"),
            ("Cursor settings", "Cursor/User"),
            ("Windsurf settings", "Windsurf/User"),
            ("VSCodium settings", "VSCodium/User"),
        ] {
            let dest = dir.replace('/', "-");
            c.add("ide", label, cfg.join(dir), &dest, true, None, &[]);
        }
        c.add("ide", "JetBrains settings", cfg.join("JetBrains"), "JetBrains", true, None, &[]);
    }
    c.add("ide", ".vscode (extensions)", home.join(".vscode"), "dot-vscode", true, Some("Extensions can be re-installed; this is mostly for reference"), &[]);
    c.add("ide", ".cursor", home.join(".cursor"), "dot-cursor", true, None, &[]);
    c.add("ide", ".windsurf", home.join(".windsurf"), "dot-windsurf", true, None, &[]);
    c.add("ide", ".codeium", home.join(".codeium"), "dot-codeium", true, None, &[]);
    c.add("ide", ".claude (Claude Code)", home.join(".claude"), "dot-claude", true, None, &[]);

    // ── Dev tools ────────────────────────────────────────────────────────
    c.add("devtools", ".docker", home.join(".docker"), "dot-docker", true, Some("config.json is stored in the encrypted secrets archive instead"), &["config.json", "sandboxes"]);
    c.add("devtools", ".serverless", home.join(".serverless"), "dot-serverless", true, None, &[]);
    c.add("devtools", ".cargo/config.toml", home.join(".cargo/config.toml"), "cargo-config.toml", true, None, &[]);
    c.add("devtools", ".kube (config)", home.join(".kube"), "dot-kube", false, Some("Often holds cluster credentials"), &[]);

    // ── Browser bookmarks ────────────────────────────────────────────────
    let mut bm = |label: &str, dest: &str, p: Option<PathBuf>| {
        if let Some(p) = p { c.add("browser", label, p, dest, true, None, &[]); }
    };
    bm("Chrome bookmarks", "Chrome-Bookmarks.json", local.as_ref().map(|l| l.join("Google/Chrome/User Data/Default/Bookmarks")));
    bm("Chrome bookmarks", "Chrome-Bookmarks.json", config.as_ref().map(|d| d.join("Google/Chrome/Default/Bookmarks")));
    bm("Edge bookmarks", "Edge-Bookmarks.json", local.as_ref().map(|l| l.join("Microsoft/Edge/User Data/Default/Bookmarks")));
    bm("Edge bookmarks", "Edge-Bookmarks.json", config.as_ref().map(|d| d.join("Microsoft Edge/Default/Bookmarks")));
    bm("Brave bookmarks", "Brave-Bookmarks.json", local.as_ref().map(|l| l.join("BraveSoftware/Brave-Browser/User Data/Default/Bookmarks")));
    bm("Brave bookmarks", "Brave-Bookmarks.json", config.as_ref().map(|d| d.join("BraveSoftware/Brave-Browser/Default/Bookmarks")));
    bm("Safari bookmarks", "Safari-Bookmarks.plist", Some(home.join("Library/Safari/Bookmarks.plist")));

    // ── Secrets (opt-in, encrypted) ──────────────────────────────────────
    let secret_note = "Stored only inside a passphrase-encrypted archive";
    c.add("secrets", ".ssh keys", home.join(".ssh"), "dot-ssh", false, Some(secret_note), &[]);
    c.add("secrets", ".aws credentials", home.join(".aws"), "dot-aws", false, Some(secret_note), &[]);
    c.add("secrets", ".gnupg keys", home.join(".gnupg"), "dot-gnupg", false, Some(secret_note), &[]);
    c.add("secrets", ".npmrc", home.join(".npmrc"), "npmrc", false, Some(secret_note), &[]);
    c.add("secrets", ".claude.json", home.join(".claude.json"), "claude.json", false, Some(secret_note), &[]);
    c.add("secrets", ".docker/config.json", home.join(".docker/config.json"), "docker-config.json", false, Some(secret_note), &[]);
    c.add("secrets", ".cargo/credentials.toml", home.join(".cargo/credentials.toml"), "cargo-credentials.toml", false, Some(secret_note), &[]);
    c.add("secrets", ".serverlessrc", home.join(".serverlessrc"), "serverlessrc", false, Some(secret_note), &[]);
    c.add("secrets", "Shell history", home.join(".zsh_history"), "zsh_history", false, Some("History can contain pasted tokens; archived encrypted"), &[]);
    c.add("secrets", "PowerShell history", config.as_ref().map(|d| d.join("Microsoft/Windows/PowerShell/PSReadLine/ConsoleHost_history.txt")).unwrap_or_default(), "powershell_history.txt", false, Some("History can contain pasted tokens; archived encrypted"), &[]);

    c.items
}

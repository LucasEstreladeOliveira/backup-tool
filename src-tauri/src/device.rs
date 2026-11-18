//! Identifies this computer so several machines can share one backup drive.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::process::Command;

#[derive(Serialize, Clone)]
pub struct Device {
    /// Short, stable ID derived from the machine's OS identifier (hashed, not the raw ID).
    pub key: String,
    pub name: String,
    pub os: String,
}

/// Content of `.backup-device.json`, stored inside each device's backup folder.
#[derive(Serialize, Deserialize, Clone)]
pub struct DeviceMarker {
    pub device_key: String,
    pub device_name: String,
    pub os: String,
    pub last_backup: String,
}

fn run(cmd: &mut Command) -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash from the GUI app
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn machine_id() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = run(Command::new("ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]))?;
        out.lines().find(|l| l.contains("IOPlatformUUID"))?.split('"').nth(3).map(str::to_string)
    } else if cfg!(windows) {
        let out = run(Command::new("reg").args(["query", r"HKLM\SOFTWARE\Microsoft\Cryptography", "/v", "MachineGuid"]))?;
        out.lines().find(|l| l.contains("MachineGuid"))?.split_whitespace().last().map(str::to_string)
    } else {
        std::fs::read_to_string("/etc/machine-id").ok().map(|s| s.trim().to_string())
    }
}

pub fn current() -> Device {
    let name = sysinfo::System::host_name().unwrap_or_else(|_| "this-computer".into());
    // Falls back to the host name when the OS offers no machine ID.
    let id = machine_id().unwrap_or_else(|| name.clone());
    let digest = Sha256::digest(format!("backup-tool-device:{id}"));
    let key: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    Device { key, name, os: std::env::consts::OS.to_string() }
}

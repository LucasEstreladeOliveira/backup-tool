# Backup Tool

A small cross-platform (Windows / macOS) desktop app, built with **Tauri 2** (Rust) and **React**, that helps you back up the files and configuration that matter to an external drive.

It scans the machine, groups what it finds into categories, lets you choose what to keep, and copies it into a dated, structured folder with a checksum manifest.

## Features

- Detects external drives and shows free space and file system (warns about FAT32's 4 GB file limit).
- Categories: documents & media, code projects, shell/terminal config, IDE settings, developer tools, browser bookmarks, secrets.
- Live size and file counts for every selectable item; blocks the backup if it will not fit.
- Skips rebuildable folders (`node_modules`, `.next`, `venv`, Rust `target`, ...) by default.
- Secrets (`.ssh`, `.aws`, tokens, shell history) are opt-in and are **only ever written inside a passphrase-encrypted [age](https://age-encryption.org) archive** - never as plain files.
- Writes `manifest.json` (source, destination, size, SHA-256) and `backup-log.txt`; a built-in **Verify** button re-hashes the copied files.
- Handles illegal filenames on FAT/NTFS targets (renames and logs them); skips symlinks and logs them.
- Optionally saves a list of installed software (`winget export` / `brew bundle dump`).

## Updating an existing backup

Each device keeps one stable folder on the drive. Running the tool again compares every file with the copy on the drive by size and modified time:

- **new** and **changed** files are copied;
- **unchanged** files are skipped and keep their hash from the previous `manifest.json`;
- nothing is ever deleted from the backup, so files you removed from the computer stay in it.

The selection screen shows, per item, how much is already backed up before you start. The secrets archive is rebuilt on each run.

## Several computers, one drive

Each computer gets its own folder, `Backup/<device name>-<key>/`. The key is a short hash of the machine's OS identifier (macOS hardware UUID, Windows `MachineGuid`, Linux `/etc/machine-id`); the raw ID is never stored. A hidden `.backup-device.json` inside the folder records the full key, name, OS and last backup time, and the tool finds a device's folder by that key - so renaming the folder is safe and one computer never touches another's files.

You can change the device name before the first backup. Older unlabelled `Backup-YYYY-MM-DD` folders are offered for adoption once, with an explicit button.

## Backup layout

```
Backup/<device name>-<key>/
  .backup-device.json
  Files/  Projects/  Terminal/  IDE/  DevTools/  Browser/
  Secrets/secrets.tar.age
  Software/
  manifest.json
  backup-log.txt
```

## Development

Requirements: Node.js 20+, Rust (stable) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.

```
npm install
npm run tauri dev      # run the app
npm run tauri build    # produce an installer for the current OS
```

Build the Windows installer on a Windows machine.

## Security notes

- The UI only sends item IDs; the Rust side resolves them to paths it discovered itself.
- A Content-Security-Policy is set, and only the `core`, `opener` and `dialog` permissions are enabled.
- `npm audit` currently reports a ReDoS in `picomatch` through Vite's build tooling. It affects development builds only, not the shipped app.
- The `.config` item skips folders that usually contain CLI tokens, but review what you copy before sharing a backup drive: plain (non-secret) categories are **not** encrypted.

## License

MIT - see [LICENSE](LICENSE).

## Not implemented yet

Restore, incremental/scheduled backups, Firefox profiles, and copying loose files in the home folder.

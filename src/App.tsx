import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import "./App.css";

type Drive = {
  name: string;
  mount: string;
  fs: string;
  total: number;
  available: number;
  removable: boolean;
  system: boolean;
};

type Item = {
  id: string;
  category: string;
  label: string;
  path: string;
  kind: string;
  default_selected: boolean;
  sensitive: boolean;
  note: string | null;
};

type Size = { bytes: number; files: number } | "error";

type Progress = {
  phase: string;
  bytes_done: number;
  bytes_total: number;
  files_done: number;
  files_total: number;
  current: string;
};

type Summary = {
  dest: string;
  files: number;
  bytes: number;
  secrets_file: string | null;
  cancelled: boolean;
  fatal: string | null;
  skipped: string[];
  skipped_total: number;
  errors: string[];
  errors_total: number;
  seconds: number;
};

type Verify = { checked: number; bad: string[] };

const CATEGORIES: { id: string; title: string; blurb: string }[] = [
  { id: "files", title: "Documents & media", blurb: "Your personal folders" },
  { id: "projects", title: "Projects", blurb: "Folders that look like code projects" },
  { id: "shell", title: "Terminal & shell", blurb: "Shell, terminal and prompt configuration" },
  { id: "ide", title: "IDEs & editors", blurb: "Settings, keybindings, snippets" },
  { id: "devtools", title: "Developer tools", blurb: "Docker, Cargo, Kubernetes and similar" },
  { id: "browser", title: "Browser bookmarks", blurb: "Chrome, Edge, Brave, Safari" },
  { id: "secrets", title: "Secrets", blurb: "Keys and credentials - encrypted with a passphrase before they touch the drive" },
];

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

function fmtTime(s: number): string {
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

const isFat = (fs: string) => ["vfat", "msdos", "fat", "fat16", "fat32"].includes(fs.toLowerCase());

export default function App() {
  const [drives, setDrives] = useState<Drive[]>([]);
  const [showSystem, setShowSystem] = useState(false);
  const [destDir, setDestDir] = useState<string>("");
  const [items, setItems] = useState<Item[]>([]);
  const [sizes, setSizes] = useState<Record<string, Size>>({});
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set(["shell", "ide", "devtools", "browser", "secrets"]));
  const [skipBuild, setSkipBuild] = useState(true);
  const [exportPkgs, setExportPkgs] = useState(true);
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");
  const [stage, setStage] = useState<"select" | "running" | "done">("select");
  const [progress, setProgress] = useState<Progress | null>(null);
  const [summary, setSummary] = useState<Summary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [verify, setVerify] = useState<Verify | "running" | null>(null);
  const startedAt = useRef(0);

  const refreshDrives = useCallback(() => invoke<Drive[]>("list_drives").then(setDrives), []);

  // Initial discovery.
  useEffect(() => {
    refreshDrives();
    invoke<Item[]>("discover").then((found) => {
      setItems(found);
      setSelected(new Set(found.filter((i) => i.default_selected).map((i) => i.id)));
    });
  }, [refreshDrives]);

  // Measure sizes (3 at a time), and again whenever "skip build folders" changes.
  useEffect(() => {
    if (items.length === 0) return;
    let cancelled = false;
    setSizes({});
    const queue = [...items];
    const worker = async () => {
      while (!cancelled) {
        const item = queue.shift();
        if (!item) return;
        try {
          const m = await invoke<{ bytes: number; files: number }>("measure", { id: item.id, skipBuild });
          if (!cancelled) setSizes((s) => ({ ...s, [item.id]: m }));
        } catch {
          if (!cancelled) setSizes((s) => ({ ...s, [item.id]: "error" }));
        }
      }
    };
    for (let i = 0; i < 3; i++) worker();
    return () => {
      cancelled = true;
    };
  }, [items, skipBuild]);

  // Backend events.
  useEffect(() => {
    const subs = [
      listen<Progress>("backup-progress", (e) => setProgress(e.payload)),
      listen<Summary>("backup-done", (e) => {
        setSummary(e.payload);
        setStage("done");
      }),
    ];
    return () => {
      subs.forEach((p) => p.then((un) => un()));
    };
  }, []);

  const drive = useMemo(() => drives.find((d) => d.mount === destDir) ?? null, [drives, destDir]);

  const totals = useMemo(() => {
    let bytes = 0;
    let files = 0;
    let pending = 0;
    for (const id of selected) {
      const s = sizes[id];
      if (!s) pending++;
      else if (s !== "error") {
        bytes += s.bytes;
        files += s.files;
      }
    }
    return { bytes, files, pending };
  }, [selected, sizes]);

  const hasSecrets = items.some((i) => i.sensitive && selected.has(i.id));
  const passOk = !hasSecrets || (pass.length >= 12 && pass === pass2);
  const tooBig = drive ? totals.bytes > drive.available : false;
  const canStart = !!destDir && selected.size > 0 && passOk && !tooBig && stage === "select";

  const toggle = (id: string) =>
    setSelected((s) => {
      const n = new Set(s);
      n.has(id) ? n.delete(id) : n.add(id);
      return n;
    });

  const toggleCategory = (cat: string, on: boolean) =>
    setSelected((s) => {
      const n = new Set(s);
      items.filter((i) => i.category === cat).forEach((i) => (on ? n.add(i.id) : n.delete(i.id)));
      return n;
    });

  const pickFolder = async () => {
    const dir = await open({ directory: true, title: "Choose where to save the backup" });
    if (typeof dir === "string") setDestDir(dir);
  };

  const start = async () => {
    setError(null);
    setVerify(null);
    setSummary(null);
    setProgress(null);
    try {
      await invoke("start_backup", {
        req: {
          dest_dir: destDir,
          item_ids: [...selected],
          skip_build: skipBuild,
          passphrase: hasSecrets ? pass : null,
          export_package_list: exportPkgs,
        },
      });
      startedAt.current = Date.now();
      setStage("running");
    } catch (e) {
      setError(String(e));
    }
  };

  const runVerify = async () => {
    if (!summary) return;
    setVerify("running");
    try {
      setVerify(await invoke<Verify>("verify_backup", { dest: summary.dest }));
    } catch (e) {
      setVerify(null);
      setError(String(e));
    }
  };

  const reset = () => {
    setStage("select");
    setSummary(null);
    setProgress(null);
    setVerify(null);
    refreshDrives();
  };

  // ── Views ─────────────────────────────────────────────────────────────
  if (stage === "running") {
    const p = progress;
    const pct = p && p.bytes_total > 0 ? Math.min(100, (p.bytes_done / p.bytes_total) * 100) : 0;
    const elapsed = (Date.now() - startedAt.current) / 1000;
    const rate = p && elapsed > 2 ? p.bytes_done / elapsed : 0;
    const eta = rate > 0 && p ? Math.round((p.bytes_total - p.bytes_done) / rate) : null;
    return (
      <main className="wrap narrow">
        <h1>{p?.phase === "scanning" ? "Scanning files…" : p?.phase === "encrypting" ? "Encrypting secrets…" : "Copying…"}</h1>
        <div className="bar">
          <div className="fill" style={{ width: `${pct}%` }} />
        </div>
        <div className="stats">
          <span>{pct.toFixed(1)}%</span>
          <span>
            {fmtBytes(p?.bytes_done ?? 0)} of {fmtBytes(p?.bytes_total ?? 0)}
          </span>
          <span>
            {(p?.files_done ?? 0).toLocaleString()} / {(p?.files_total ?? 0).toLocaleString()} files
          </span>
          {rate > 0 && <span>{fmtBytes(rate)}/s</span>}
          {eta !== null && <span>~{fmtTime(eta)} left</span>}
        </div>
        <p className="current" title={p?.current}>
          {p?.current}
        </p>
        <button className="danger" onClick={() => invoke("cancel_backup")}>
          Cancel
        </button>
      </main>
    );
  }

  if (stage === "done" && summary) {
    const ok = !summary.fatal && !summary.cancelled && summary.errors_total === 0;
    return (
      <main className="wrap narrow">
        <h1>{summary.fatal ? "Backup could not start" : summary.cancelled ? "Backup cancelled" : ok ? "Backup complete" : "Backup finished with problems"}</h1>
        {summary.fatal && <p className="err">{summary.fatal}</p>}
        {!summary.fatal && (
          <p>
            {summary.files.toLocaleString()} files, {fmtBytes(summary.bytes)} in {fmtTime(summary.seconds)}.
            {summary.secrets_file && " Secrets are saved in an encrypted archive."}
          </p>
        )}
        <p className="path">{summary.dest}</p>
        <div className="row">
          <button onClick={() => revealItemInDir(summary.dest)}>Show in folder</button>
          {!summary.fatal && (
            <button onClick={runVerify} disabled={verify === "running"}>
              {verify === "running" ? "Verifying…" : "Verify copied files"}
            </button>
          )}
          <button className="ghost" onClick={reset}>
            Back
          </button>
        </div>
        {verify && verify !== "running" && (
          <p className={verify.bad.length ? "err" : "okmsg"}>
            {verify.bad.length ? `${verify.bad.length} of ${verify.checked} files failed verification.` : `All ${verify.checked.toLocaleString()} files verified.`}
          </p>
        )}
        {verify && verify !== "running" && verify.bad.length > 0 && <Details title="Failed files" lines={verify.bad} total={verify.bad.length} />}
        {summary.errors_total > 0 && <Details title={`Errors (${summary.errors_total})`} lines={summary.errors} total={summary.errors_total} />}
        {summary.skipped_total > 0 && <Details title={`Notes and skipped items (${summary.skipped_total})`} lines={summary.skipped} total={summary.skipped_total} />}
        {error && <p className="err">{error}</p>}
      </main>
    );
  }

  const visibleDrives = drives.filter((d) => showSystem || !d.system);

  return (
    <main className="wrap">
      <header>
        <h1>Backup Tool</h1>
        <p className="sub">Pick a destination, choose what to keep, and copy it in a tidy, verifiable structure.</p>
      </header>

      <section className="card">
        <h2>1 · Destination</h2>
        <div className="drives">
          {visibleDrives.length === 0 && <p className="muted">No external drives found. Plug one in, or choose a folder.</p>}
          {visibleDrives.map((d) => (
            <label key={d.mount} className={`drive ${destDir === d.mount ? "on" : ""}`}>
              <input type="radio" name="drive" checked={destDir === d.mount} onChange={() => setDestDir(d.mount)} />
              <div>
                <strong>{d.name || d.mount}</strong> <span className="muted">{d.mount}</span>
                <div className="muted small">
                  {fmtBytes(d.available)} free of {fmtBytes(d.total)} · {d.fs || "unknown format"}
                  {d.removable ? " · removable" : ""}
                  {d.system ? " · system drive" : ""}
                </div>
                {isFat(d.fs) && <div className="warn small">FAT32: files over 4 GB will be skipped and listed in the log.</div>}
              </div>
            </label>
          ))}
        </div>
        <div className="row">
          <button className="ghost" onClick={pickFolder}>
            Choose another folder…
          </button>
          <button className="ghost" onClick={refreshDrives}>
            Refresh drives
          </button>
          <label className="check">
            <input type="checkbox" checked={showSystem} onChange={(e) => setShowSystem(e.target.checked)} /> Show system drive
          </label>
        </div>
        {destDir && !drive && <p className="muted small">Saving into: {destDir}</p>}
      </section>

      <section className="card">
        <h2>2 · What to back up</h2>
        {items.length === 0 && <p className="muted">Scanning this computer…</p>}
        {CATEGORIES.map((cat) => {
          const list = items.filter((i) => i.category === cat.id);
          if (list.length === 0) return null;
          const chosen = list.filter((i) => selected.has(i.id));
          const sized = chosen.reduce((a, i) => {
            const s = sizes[i.id];
            return a + (s && s !== "error" ? s.bytes : 0);
          }, 0);
          const open_ = !collapsed.has(cat.id);
          return (
            <div key={cat.id} className="cat">
              <div className="cathead">
                <input
                  type="checkbox"
                  checked={chosen.length === list.length}
                  ref={(el) => {
                    if (el) el.indeterminate = chosen.length > 0 && chosen.length < list.length;
                  }}
                  onChange={(e) => toggleCategory(cat.id, e.target.checked)}
                />
                <button
                  className="expander"
                  onClick={() =>
                    setCollapsed((c) => {
                      const n = new Set(c);
                      n.has(cat.id) ? n.delete(cat.id) : n.add(cat.id);
                      return n;
                    })
                  }
                >
                  <span>{open_ ? "▾" : "▸"}</span> <strong>{cat.title}</strong>
                  <span className="muted small"> {cat.blurb}</span>
                </button>
                <span className="muted small">
                  {chosen.length}/{list.length} · {fmtBytes(sized)}
                </span>
              </div>
              {open_ &&
                list.map((i) => {
                  const s = sizes[i.id];
                  return (
                    <label key={i.id} className="item">
                      <input type="checkbox" checked={selected.has(i.id)} onChange={() => toggle(i.id)} />
                      <div className="grow">
                        <div>
                          {i.label} {i.sensitive && <span className="badge">encrypted</span>}
                        </div>
                        <div className="muted small path">{i.path}</div>
                        {i.note && <div className="muted small">{i.note}</div>}
                      </div>
                      <span className="muted small size">
                        {s === undefined ? "measuring…" : s === "error" ? "?" : `${fmtBytes(s.bytes)} · ${s.files.toLocaleString()} files`}
                      </span>
                    </label>
                  );
                })}
            </div>
          );
        })}
      </section>

      <section className="card">
        <h2>3 · Options</h2>
        <label className="check">
          <input type="checkbox" checked={skipBuild} onChange={(e) => setSkipBuild(e.target.checked)} /> Skip rebuildable folders (node_modules, .next, venv, Rust target…)
        </label>
        <label className="check">
          <input type="checkbox" checked={exportPkgs} onChange={(e) => setExportPkgs(e.target.checked)} /> Save a list of installed software (winget / Homebrew) so it can be reinstalled
        </label>
        {hasSecrets && (
          <div className="pass">
            <p className="warn small">Secrets are encrypted with this passphrase. If you lose it, they cannot be recovered.</p>
            <input type="password" placeholder="Passphrase (12+ characters)" value={pass} onChange={(e) => setPass(e.target.value)} />
            <input type="password" placeholder="Repeat passphrase" value={pass2} onChange={(e) => setPass2(e.target.value)} />
            {pass2 && pass !== pass2 && <span className="err small">Passphrases don’t match</span>}
          </div>
        )}
      </section>

      <footer className="bar-bottom">
        <div>
          <strong>{fmtBytes(totals.bytes)}</strong> · {totals.files.toLocaleString()} files selected
          {totals.pending > 0 && <span className="muted"> (measuring {totals.pending} more…)</span>}
          {drive && <span className={tooBig ? "err" : "muted"}> · {fmtBytes(drive.available)} free on drive</span>}
          {tooBig && <span className="err"> — does not fit</span>}
        </div>
        {error && <span className="err small">{error}</span>}
        <button className="primary" disabled={!canStart} onClick={start}>
          Start backup
        </button>
      </footer>
    </main>
  );
}

function Details({ title, lines, total }: { title: string; lines: string[]; total: number }) {
  return (
    <details className="details">
      <summary>{title}</summary>
      <pre>
        {lines.join("\n")}
        {total > lines.length ? `\n… and ${total - lines.length} more (see backup-log.txt)` : ""}
      </pre>
    </details>
  );
}

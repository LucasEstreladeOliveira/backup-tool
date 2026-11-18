import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { CSSProperties, ReactNode } from "react";
import {
  ArrowRight, Check, ChevronDown, CircleAlert, CircleCheck, Code, FileText, FolderGit2, FolderOpen, Globe, HardDrive, History, Info,
  KeyRound, Laptop, LoaderCircle, Lock, Monitor, RefreshCw, RotateCcw, ShieldCheck, Terminal, TriangleAlert, Usb, Wrench, X,
  type LucideIcon,
} from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

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

type Device = { key: string; name: string; os: string };
type DeviceEntry = { folder: string; name: string; os: string; last_backup: string; this_device: boolean };

type Preview = {
  new_files: number;
  new_bytes: number;
  updated_files: number;
  updated_bytes: number;
  unchanged_files: number;
  unchanged_bytes: number;
};

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
  new_files: number;
  updated_files: number;
  unchanged_files: number;
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
  const [status, setStatus] = useState<Record<string, Preview>>({});
  const [existing, setExisting] = useState<string | null>(null);
  const [device, setDevice] = useState<Device | null>(null);
  const [label, setLabel] = useState("");
  const [devices, setDevices] = useState<DeviceEntry[]>([]);
  const [legacy, setLegacy] = useState<string[]>([]);
  const [refreshKey, setRefreshKey] = useState(0);
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
  const [shakeKey, setShakeKey] = useState(0);
  const [verify, setVerify] = useState<Verify | "running" | null>(null);
  const startedAt = useRef(0);

  const refreshDrives = useCallback(() => invoke<Drive[]>("list_drives").then(setDrives), []);

  // Initial discovery.
  useEffect(() => {
    refreshDrives();
    invoke<Device>("device_info").then((d) => {
      setDevice(d);
      setLabel(d.name);
    });
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

  // Compare every item with what is already on the destination (3 at a time).
  useEffect(() => {
    setStatus({});
    setExisting(null);
    if (!destDir || items.length === 0) return;
    let cancelled = false;
    invoke<string | null>("existing_backup", { destDir }).then((p) => !cancelled && setExisting(p));
    const queue = items.filter((i) => !i.sensitive);
    const worker = async () => {
      while (!cancelled) {
        const item = queue.shift();
        if (!item) return;
        try {
          const p = await invoke<Preview>("preview_item", { id: item.id, destDir, skipBuild });
          if (!cancelled) setStatus((s) => ({ ...s, [item.id]: p }));
        } catch {
          /* item stays on its plain size */
        }
      }
    };
    for (let i = 0; i < 3; i++) worker();
    return () => {
      cancelled = true;
    };
  }, [items, destDir, skipBuild, refreshKey]);

  // Who else has a backup on this drive, and are there older unlabelled backups to claim?
  useEffect(() => {
    setDevices([]);
    setLegacy([]);
    if (!destDir) return;
    let cancelled = false;
    invoke<DeviceEntry[]>("list_devices", { destDir }).then((d) => !cancelled && setDevices(d));
    invoke<string[]>("legacy_backups", { destDir }).then((l) => !cancelled && setLegacy(l));
    return () => {
      cancelled = true;
    };
  }, [destDir, refreshKey, stage]);

  // A backup may already be running (for example after the window reloaded): take over.
  useEffect(() => {
    invoke<{ running: boolean; started_ms: number; progress: Progress | null }>("backup_status")
      .then((st) => {
        if (!st.running) return;
        startedAt.current = st.started_ms || Date.now();
        setProgress(st.progress);
        setStage("running");
      })
      .catch(() => {});
  }, []);

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
    let unchanged = 0;
    for (const id of selected) {
      const p = status[id];
      const s = sizes[id];
      if (p) {
        bytes += p.new_bytes + p.updated_bytes;
        files += p.new_files + p.updated_files;
        unchanged += p.unchanged_files;
      } else if (!s) pending++;
      else if (s !== "error") {
        bytes += s.bytes;
        files += s.files;
      }
    }
    return { bytes, files, pending, unchanged };
  }, [selected, sizes, status]);

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
          device_label: label.trim() || null,
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

  const adopt = async (folder: string) => {
    setError(null);
    try {
      await invoke("adopt_legacy", { destDir, legacy: folder, label: label.trim() || null });
      setRefreshKey((k) => k + 1);
    } catch (e) {
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
    const phase = p?.phase === "scanning" ? "Scanning files" : p?.phase === "encrypting" ? "Encrypting secrets" : "Copying";
    return (
      <main className="stage-enter mx-auto flex min-h-full max-w-xl flex-col justify-center gap-6 px-6 py-10">
        <div className="flex flex-col gap-1.5">
          <span className="shimmer text-sm font-medium">{phase}…</span>
          <h1 className="flex items-baseline gap-1 text-5xl font-semibold tracking-tight tnum">
            <Num value={pct.toFixed(1)} />
            <span className="text-2xl text-muted-foreground">%</span>
          </h1>
        </div>
        <Progress value={pct} aria-label="Backup progress" />
        <dl className="grid grid-cols-2 gap-px overflow-hidden rounded-xl border bg-border sm:grid-cols-4">
          <Stat label="Copied" value={`${fmtBytes(p?.bytes_done ?? 0)}`} hint={`of ${fmtBytes(p?.bytes_total ?? 0)}`} />
          <Stat label="Files" value={(p?.files_done ?? 0).toLocaleString()} hint={`of ${(p?.files_total ?? 0).toLocaleString()}`} />
          <Stat label="Speed" value={rate > 0 ? `${fmtBytes(rate)}/s` : "-"} />
          <Stat label="Left" value={eta !== null ? `~${fmtTime(eta)}` : "-"} />
        </dl>
        <p className="truncate text-left font-mono text-xs text-muted-foreground" style={{ direction: "rtl" }} title={p?.current}>
          {p?.current || " "}
        </p>
        <div>
          <Button variant="destructive" size="lg" onClick={() => invoke("cancel_backup")}>
            <X /> Cancel backup
          </Button>
        </div>
      </main>
    );
  }

  if (stage === "done" && summary) {
    const ok = !summary.fatal && !summary.cancelled && summary.errors_total === 0;
    const title = summary.fatal ? "Backup could not start" : summary.cancelled ? "Backup cancelled" : ok ? "Backup complete" : "Backup finished with problems";
    return (
      <main className="stage-enter mx-auto flex min-h-full max-w-xl flex-col justify-center gap-6 px-6 py-10">
        <div className="flex items-center gap-4">
          {ok ? (
            <span className="ok-badge grid size-14 shrink-0 place-items-center rounded-full bg-(--success) text-background">
              <svg viewBox="0 0 24 24" className="size-7" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                <path d="M5 12.5l4.5 4.5L19 7.5" />
              </svg>
            </span>
          ) : (
            <span className="grid size-14 shrink-0 place-items-center rounded-full bg-destructive/10 text-destructive">
              <TriangleAlert className="size-7" />
            </span>
          )}
          <div className="min-w-0">
            <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
            {summary.fatal ? (
              <p className="text-sm text-destructive">{summary.fatal}</p>
            ) : (
              <p className="text-sm text-muted-foreground">
                {fmtBytes(summary.bytes)} written in {fmtTime(summary.seconds)}
                {summary.secrets_file && " · secrets saved in an encrypted archive"}
              </p>
            )}
          </div>
        </div>

        {!summary.fatal && (
          <dl className="grid grid-cols-3 gap-px overflow-hidden rounded-xl border bg-border">
            <Stat label="New" value={summary.new_files.toLocaleString()} />
            <Stat label="Updated" value={summary.updated_files.toLocaleString()} />
            <Stat label="Already there" value={summary.unchanged_files.toLocaleString()} />
          </dl>
        )}

        <p className="break-all rounded-lg border bg-muted/60 px-3 py-2 font-mono text-xs text-muted-foreground">{summary.dest}</p>

        <div className="flex flex-wrap gap-2">
          <Button size="lg" onClick={() => revealItemInDir(summary.dest)}>
            <FolderOpen /> Show in folder
          </Button>
          {!summary.fatal && (
            <Button variant="outline" size="lg" onClick={runVerify} disabled={verify === "running"}>
              {verify === "running" ? <LoaderCircle className="animate-spin" /> : <ShieldCheck />}
              {verify === "running" ? "Verifying…" : "Verify copied files"}
            </Button>
          )}
          <Button variant="ghost" size="lg" onClick={reset}>
            <RotateCcw /> Back
          </Button>
        </div>

        <Reveal open={!!verify && verify !== "running"}>
          {verify && verify !== "running" && (
            <Alert variant={verify.bad.length ? "destructive" : "default"}>
              {verify.bad.length ? <CircleAlert /> : <CircleCheck className="text-(--success)" />}
              <AlertTitle>{verify.bad.length ? `${verify.bad.length} of ${verify.checked.toLocaleString()} files failed verification` : `All ${verify.checked.toLocaleString()} files verified`}</AlertTitle>
            </Alert>
          )}
        </Reveal>
        {verify && verify !== "running" && verify.bad.length > 0 && <Details title="Failed files" lines={verify.bad} total={verify.bad.length} />}
        {summary.errors_total > 0 && <Details title={`Errors (${summary.errors_total})`} lines={summary.errors} total={summary.errors_total} />}
        {summary.skipped_total > 0 && <Details title={`Notes and skipped items (${summary.skipped_total})`} lines={summary.skipped} total={summary.skipped_total} />}
        {error && <p className="text-sm text-destructive">{error}</p>}
      </main>
    );
  }

  const visibleDrives = drives.filter((d) => showSystem || !d.system);
  const blocker = !destDir
    ? "Choose where to save the backup"
    : selected.size === 0
      ? "Select something to back up"
      : !passOk
        ? "Set a passphrase for your secrets"
        : tooBig
          ? "Not enough space on this drive"
          : null;

  return (
    <div className="stage-enter min-h-full">
      <main className="mx-auto flex max-w-[880px] flex-col gap-5 px-6 pb-36 pt-10">
        <header className="flex items-start justify-between gap-4">
          <div className="flex flex-col gap-1.5">
            <h1 className="rise text-3xl font-semibold tracking-tight" style={{ "--i": 0 } as CSSProperties}>
              Backup Tool
            </h1>
            <p className="rise max-w-md text-balance text-muted-foreground" style={{ "--i": 1 } as CSSProperties}>
              Pick a drive, choose what to keep, and copy it into a folder you can verify later.
            </p>
          </div>
          {device && (
            <Badge variant="secondary" className="rise h-7 gap-1.5 px-2.5 text-xs" style={{ "--i": 2 } as CSSProperties}>
              <Laptop /> {device.name}
            </Badge>
          )}
        </header>

        {/* 1 · Destination */}
        <Step n={1} title="Destination" done={!!destDir}>
          {visibleDrives.length === 0 && <p className="text-sm text-muted-foreground">No external drives found. Plug one in, or choose a folder.</p>}
          <RadioGroup value={destDir} onValueChange={(v) => setDestDir(String(v))} aria-label="Backup destination">
            {visibleDrives.map((d) => {
              const on = destDir === d.mount;
              return (
                <label
                  key={d.mount}
                  className="lift flex cursor-pointer items-start gap-3 rounded-xl border bg-card p-3.5 hover:border-primary/40 has-[[data-checked]]:border-primary has-[[data-checked]]:bg-accent/60"
                >
                  <RadioGroupItem value={d.mount} className="mt-1" />
                  <span className="mt-0.5 grid size-8 shrink-0 place-items-center rounded-lg bg-muted text-muted-foreground">
                    {d.removable ? <Usb className="size-4" /> : <HardDrive className="size-4" />}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-baseline gap-x-2">
                      <span className="font-medium">{d.name || d.mount}</span>
                      <span className="truncate text-xs text-muted-foreground">{d.mount}</span>
                      {d.system && <Badge variant="outline">system drive</Badge>}
                    </div>
                    <div className="text-xs text-muted-foreground tnum">
                      {fmtBytes(d.available)} free of {fmtBytes(d.total)} · {d.fs || "unknown format"}
                      {d.removable ? " · removable" : ""}
                    </div>
                    <DriveMeter drive={d} add={on ? totals.bytes : 0} />
                    {isFat(d.fs) && (
                      <div className="mt-1.5 flex items-center gap-1.5 text-xs text-(--warn)">
                        <TriangleAlert className="size-3.5" /> FAT32: files over 4 GB will be skipped and listed in the log.
                      </div>
                    )}
                  </div>
                </label>
              );
            })}
          </RadioGroup>

          {destDir && !drive && (
            <div className="flex items-center gap-2 rounded-xl border border-primary bg-accent/60 p-3 text-sm">
              <FolderOpen className="size-4 shrink-0 text-primary" />
              <span className="min-w-0 truncate" title={destDir}>{destDir}</span>
            </div>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" onClick={pickFolder}>
              <FolderOpen /> Choose another folder
            </Button>
            <Tooltip>
              <TooltipTrigger render={<Button variant="ghost" size="icon" aria-label="Refresh drives" onClick={refreshDrives} />}>
                <RefreshCw />
              </TooltipTrigger>
              <TooltipContent>Refresh drives</TooltipContent>
            </Tooltip>
            <div className="ml-auto flex items-center gap-2">
              <Switch id="show-system" size="sm" checked={showSystem} onCheckedChange={setShowSystem} />
              <Label htmlFor="show-system" className="text-sm text-muted-foreground">Show system drive</Label>
            </div>
          </div>

          <Reveal open={!!destDir}>
            <div className="flex flex-col gap-3 border-t pt-4">
              <div className="flex flex-wrap items-center gap-3">
                <Label htmlFor="device-label" className="text-sm">This device is called</Label>
                <Input id="device-label" className="h-8 w-56" value={label} onChange={(e) => setLabel(e.target.value)} placeholder={device?.name} disabled={!!existing} />
                <span className="text-xs text-muted-foreground">key {device?.key}</span>
              </div>
              {devices.length > 0 && (
                <ul className="flex flex-col gap-1 text-xs text-muted-foreground">
                  {devices.map((d) => (
                    <li key={d.folder} className="flex items-center gap-2">
                      <Monitor className="size-3.5" />
                      {d.name} ({d.os}) · last backup {d.last_backup}
                      {d.this_device && <Badge>this device</Badge>}
                    </li>
                  ))}
                </ul>
              )}
              {!existing &&
                legacy.map((f) => (
                  <Alert key={f}>
                    <History />
                    <AlertTitle>Earlier backup without a device label</AlertTitle>
                    <AlertDescription>
                      <span className="flex flex-wrap items-center gap-2">
                        {f.split(/[\\/]/).pop()}
                        <Button variant="outline" size="xs" onClick={() => adopt(f)}>It is from this device - use it</Button>
                      </span>
                    </AlertDescription>
                  </Alert>
                ))}
              <p className="flex items-start gap-2 text-sm text-muted-foreground">
                {existing ? <History className="mt-0.5 size-4 shrink-0 text-primary" /> : <Info className="mt-0.5 size-4 shrink-0" />}
                <span className="min-w-0 break-words">
                  {existing ? `Existing backup found - only new and changed files will be copied: ${existing}` : "No earlier backup here - this will be a full backup."}
                </span>
              </p>
            </div>
          </Reveal>
        </Step>

        {/* 2 · What to back up */}
        <Step n={2} title="What to back up" done={selected.size > 0} aside={`${selected.size} of ${items.length} selected`}>
          {items.length === 0 && <p className="shimmer w-fit text-sm">Scanning this computer…</p>}
          <div className="divide-y overflow-hidden rounded-xl border bg-card">
            {CATEGORIES.map((cat) => {
              const list = items.filter((i) => i.category === cat.id);
              if (list.length === 0) return null;
              const chosen = list.filter((i) => selected.has(i.id));
              const sized = chosen.reduce((a, i) => {
                const s = sizes[i.id];
                return a + (s && s !== "error" ? s.bytes : 0);
              }, 0);
              const isOpen = !collapsed.has(cat.id);
              const Icon = CATEGORY_ICONS[cat.id] ?? FileText;
              return (
                <Collapsible
                  key={cat.id}
                  open={isOpen}
                  onOpenChange={() =>
                    setCollapsed((c) => {
                      const n = new Set(c);
                      n.has(cat.id) ? n.delete(cat.id) : n.add(cat.id);
                      return n;
                    })
                  }
                >
                  <div className="t-acc" data-open={isOpen}>
                    <div className="flex items-center gap-3 px-3.5 py-2.5 transition-colors hover:bg-muted/50">
                      <Checkbox
                        aria-label={`Select all in ${cat.title}`}
                        checked={chosen.length === list.length}
                        indeterminate={chosen.length > 0 && chosen.length < list.length}
                        onCheckedChange={(v) => toggleCategory(cat.id, v)}
                      />
                      <CollapsibleTrigger className="flex min-w-0 flex-1 items-center gap-3 rounded-md py-1 text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/50">
                        <Icon className="size-4 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1">
                          <span className="block font-medium leading-tight">{cat.title}</span>
                          <span className="block truncate text-xs text-muted-foreground">{cat.blurb}</span>
                        </span>
                        <span className="shrink-0 text-xs text-muted-foreground tnum">
                          {chosen.length}/{list.length} · <Num value={fmtBytes(sized)} />
                        </span>
                        <span className="t-acc-chevron text-muted-foreground">
                          <ChevronDown className="size-4" />
                        </span>
                      </CollapsibleTrigger>
                    </div>
                    <div className="t-acc-panel">
                      <div className="t-acc-panel-inner" inert={!isOpen}>
                        <div className="flex flex-col gap-0.5 border-t bg-muted/30 p-1.5">
                          {list.map((i) => (
                            <label key={i.id} className="flex cursor-pointer items-start gap-3 rounded-lg px-2.5 py-2 transition-colors hover:bg-card">
                              <Checkbox className="mt-0.5" checked={selected.has(i.id)} onCheckedChange={() => toggle(i.id)} />
                              <div className="min-w-0 flex-1">
                                <div className="flex items-center gap-2 text-sm">
                                  {i.label}
                                  {i.sensitive && (
                                    <Badge variant="outline" className="gap-1 text-(--warn)">
                                      <Lock /> encrypted
                                    </Badge>
                                  )}
                                </div>
                                <div className="truncate font-mono text-[11px] text-muted-foreground" title={i.path}>{i.path}</div>
                                {i.note && <div className="text-xs text-muted-foreground">{i.note}</div>}
                              </div>
                              <span className="shrink-0 pt-0.5 text-right text-xs text-muted-foreground tnum">
                                <SizeText s={sizes[i.id]} p={status[i.id]} />
                              </span>
                            </label>
                          ))}
                        </div>
                      </div>
                    </div>
                  </div>
                </Collapsible>
              );
            })}
          </div>
        </Step>

        {/* 3 · Options */}
        <Step n={3} title="Options" done>
          <div className="flex flex-col gap-3">
            <OptionRow id="skip-build" checked={skipBuild} onChange={setSkipBuild} title="Skip rebuildable folders" hint="node_modules, .next, venv, Rust target and similar" />
            <OptionRow id="export-pkgs" checked={exportPkgs} onChange={setExportPkgs} title="Save a list of installed software" hint="winget or Homebrew, so you can reinstall it later" />
          </div>
          <Reveal open={hasSecrets}>
            <div className="flex flex-col gap-3 border-t pt-4">
              <Alert>
                <Lock />
                <AlertTitle>Secrets are encrypted with this passphrase</AlertTitle>
                <AlertDescription>If you lose it, they cannot be recovered.</AlertDescription>
              </Alert>
              <div className="grid gap-3 sm:grid-cols-2">
                <Input type="password" aria-label="Passphrase" placeholder="Passphrase (12+ characters)" value={pass} onChange={(e) => setPass(e.target.value)} />
                <div key={shakeKey} className={shakeKey ? "shake" : undefined}>
                  <Input
                    type="password"
                    aria-label="Repeat passphrase"
                    placeholder="Repeat passphrase"
                    value={pass2}
                    aria-invalid={!!pass2 && pass !== pass2}
                    onChange={(e) => setPass2(e.target.value)}
                    onBlur={() => pass2 && pass !== pass2 && setShakeKey((k) => k + 1)}
                  />
                </div>
              </div>
              {pass2 && pass !== pass2 && <p className="text-xs text-destructive">Passphrases don’t match</p>}
              {pass && pass.length < 12 && <p className="text-xs text-muted-foreground">{12 - pass.length} more characters needed</p>}
            </div>
          </Reveal>
        </Step>
      </main>

      {/* Summary + action */}
      <footer className="fixed inset-x-0 bottom-0 z-10 border-t bg-background/85 backdrop-blur-md">
        <div className="mx-auto flex max-w-[880px] items-center justify-between gap-4 px-6 py-3.5">
          <div className="min-w-0">
            <div className="flex items-baseline gap-2">
              <span className="text-xl font-semibold tracking-tight tnum">
                <Num value={fmtBytes(totals.bytes)} />
              </span>
              <span className="text-sm text-muted-foreground tnum">
                <Num value={`${totals.files.toLocaleString()} files to copy`} />
              </span>
            </div>
            <div className="truncate text-xs text-muted-foreground tnum">
              {totals.unchanged > 0 && <span className="text-(--success)">{totals.unchanged.toLocaleString()} already backed up · </span>}
              {totals.pending > 0 && <span>measuring {totals.pending} more… · </span>}
              {drive ? (
                <span className={tooBig ? "text-destructive" : undefined}>
                  {fmtBytes(drive.available)} free on drive{tooBig ? " - does not fit" : ""}
                </span>
              ) : (
                <span>{blocker ?? "Ready"}</span>
              )}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-3">
            {error && <span className="max-w-xs truncate text-xs text-destructive" title={error}>{error}</span>}
            {blocker && drive && <span className="hidden text-xs text-muted-foreground sm:inline">{blocker}</span>}
            <Button size="lg" className="h-10 px-5 text-sm font-semibold" disabled={!canStart} onClick={start}>
              Start backup <ArrowRight data-icon="inline-end" />
            </Button>
          </div>
        </div>
      </footer>
    </div>
  );
}

// ── Pieces ──────────────────────────────────────────────────────────────

const CATEGORY_ICONS: Record<string, LucideIcon> = {
  files: FileText,
  projects: FolderGit2,
  shell: Terminal,
  ide: Code,
  devtools: Wrench,
  browser: Globe,
  secrets: KeyRound,
};

/** A value that fades up in place whenever it changes. */
function Num({ value }: { value: string }) {
  return (
    <span key={value} className="num-in">
      {value}
    </span>
  );
}

/** Collapsing region: grid-rows 0fr -> 1fr, so any content height animates. */
function Reveal({ open, children }: { open: boolean; children: ReactNode }) {
  return (
    <div className="t-acc" data-open={open}>
      <div className="t-acc-panel">
        <div className="t-acc-panel-inner" inert={!open}>
          {children}
        </div>
      </div>
    </div>
  );
}

function Step({ n, title, done, aside, children }: { n: number; title: string; done: boolean; aside?: string; children: ReactNode }) {
  return (
    <Card className="gap-4 py-5">
      <div className="flex items-center gap-3 px-4">
        <span className="relative grid size-6 shrink-0 place-items-center rounded-full border text-xs font-medium tnum transition-colors duration-250 data-[done=true]:border-primary data-[done=true]:bg-primary data-[done=true]:text-primary-foreground" data-done={done}>
          <span className="absolute transition-all duration-250 data-[on=false]:scale-50 data-[on=false]:opacity-0 data-[on=false]:blur-[2px]" data-on={!done}>{n}</span>
          <Check className="absolute size-3.5 transition-all duration-250" style={{ opacity: done ? 1 : 0, transform: done ? "none" : "scale(0.5)", filter: done ? "none" : "blur(2px)" }} />
        </span>
        <CardTitle className="text-base font-semibold">{title}</CardTitle>
        {aside && <span className="ml-auto text-xs text-muted-foreground tnum">{aside}</span>}
      </div>
      <CardContent className="flex flex-col gap-3">{children}</CardContent>
    </Card>
  );
}

function OptionRow({ id, checked, onChange, title, hint }: { id: string; checked: boolean; onChange: (v: boolean) => void; title: string; hint: string }) {
  return (
    <div className="flex items-center gap-3">
      <Switch id={id} checked={checked} onCheckedChange={onChange} />
      <Label htmlFor={id} className="flex flex-col items-start gap-0 text-left">
        <span className="text-sm font-medium">{title}</span>
        <span className="text-xs font-normal text-muted-foreground">{hint}</span>
      </Label>
    </div>
  );
}

function DriveMeter({ drive, add }: { drive: Drive; add: number }) {
  if (drive.total <= 0) return null;
  const used = ((drive.total - drive.available) / drive.total) * 100;
  const incoming = Math.min(add, drive.available) / drive.total * 100;
  const over = add > drive.available;
  return (
    <div className="mt-2 flex h-1.5 w-full overflow-hidden rounded-full bg-muted" role="img" aria-label={`${used.toFixed(0)}% used`}>
      <div className="meter-seg h-full bg-foreground/25" style={{ width: `${used}%` }} />
      <div className={`meter-seg h-full ${over ? "bg-destructive" : "bg-primary"}`} style={{ width: `${incoming}%` }} />
    </div>
  );
}

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="flex flex-col gap-0.5 bg-card px-4 py-3">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-lg font-semibold tracking-tight tnum">
        <Num value={value} />
        {hint && <span className="ml-1.5 text-xs font-normal text-muted-foreground">{hint}</span>}
      </dd>
    </div>
  );
}

function SizeText({ s, p }: { s: Size | undefined; p: Preview | undefined }) {
  if (!p && s === undefined) return <span className="shimmer">measuring…</span>;
  const text = sizeText(s, p);
  const done = p && p.new_files + p.updated_files === 0 && p.unchanged_files > 0;
  return <span className={done ? "inline-flex items-center gap-1 text-(--success)" : undefined}>{done && <Check className="size-3" />}<Num value={done ? "already backed up" : text} /></span>;
}

function sizeText(s: Size | undefined, p: Preview | undefined): string {
  if (p) {
    const todo = p.new_files + p.updated_files;
    if (todo === 0) return p.unchanged_files > 0 ? "already backed up" : "empty";
    const parts = [`${p.new_files.toLocaleString()} new`];
    if (p.updated_files > 0) parts.push(`${p.updated_files.toLocaleString()} changed`);
    parts.push(`${fmtBytes(p.new_bytes + p.updated_bytes)} to copy`);
    if (p.unchanged_files > 0) parts.push(`${p.unchanged_files.toLocaleString()} up to date`);
    return parts.join(" · ");
  }
  if (s === undefined) return "measuring…";
  return s === "error" ? "?" : `${fmtBytes(s.bytes)} · ${s.files.toLocaleString()} files`;
}

function Details({ title, lines, total }: { title: string; lines: string[]; total: number }) {
  const [open, setOpen] = useState(false);
  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <div className="t-acc overflow-hidden rounded-xl border bg-card" data-open={open}>
        <CollapsibleTrigger className="flex w-full items-center justify-between px-3.5 py-2.5 text-left text-sm font-medium outline-none transition-colors hover:bg-muted/50 focus-visible:ring-3 focus-visible:ring-ring/50">
          {title}
          <span className="t-acc-chevron text-muted-foreground">
            <ChevronDown className="size-4" />
          </span>
        </CollapsibleTrigger>
        <div className="t-acc-panel">
          <div className="t-acc-panel-inner" inert={!open}>
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all border-t bg-muted/30 p-3 font-mono text-xs">
              {lines.join("\n")}
              {total > lines.length ? `\n… and ${total - lines.length} more (see backup-log.txt)` : ""}
            </pre>
          </div>
        </div>
      </div>
    </Collapsible>
  );
}

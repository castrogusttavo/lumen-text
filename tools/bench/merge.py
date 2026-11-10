#!/usr/bin/env python3
"""Merge the raw results in <bench>/out into web/src/data/benchmarks.json (schema kept).

Only cells/metrics we measured are touched; editors without result files keep their old cells.
Every number is a median over N runs with min/max in the note; anything that cannot be measured
is {"value": null, "source": "unavailable", "reason": "..."} (new, backwards-compatible field).
Usage: merge.py <bench dir> <benchmarks.json> <results archive dir>
"""
import datetime, json, os, platform, re, shutil, subprocess, sys

bench, target, archive = sys.argv[1:4]
out = f"{bench}/out"
home = os.path.expanduser("~")


def load(name):
    p = f"{out}/{name}.json"
    return json.load(open(p)) if os.path.exists(p) else None


def sh(cmd):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return ""


def fm(v, d=0):
    return f"{v:.{d}f}"


def rng(s, d=0, unit=""):
    return f"median {fm(s['median'], d)}{unit} (min {fm(s['min'], d)}, max {fm(s['max'], d)}, N={s['n']})"


bj = json.load(open(target))
editors = {e["id"]: e for e in bj["editors"]}
metric = {m["id"]: m for m in bj["metrics"]}
IDS = ["lumen", "vscode", "zed", "sublime", "openidea"]


def cell(mid, ed, value, note, source="measured", reason=None):
    c = {"value": None if value is None else round(value, 1 if abs(value) < 100 else 0), "source": source}
    if reason:
        c["reason"] = reason
    if note:
        c["note"] = note
    metric[mid]["values"][ed] = c


def unavailable(mid, ed, reason):
    metric[mid]["values"][ed] = {"value": None, "source": "unavailable", "reason": reason}


# ---------------------------------------------------------------- machine + versions
cpu = re.sub(r"\s+", " ", sh("lscpu | grep -m1 'Model name' | cut -d: -f2")).replace("(R)", "").replace("(TM)", "")
mem_kb = int(sh("awk '/MemTotal/{print $2}' /proc/meminfo") or 0)
disk_model = sh("lsblk -dno MODEL $(findmnt -no SOURCE / | sed 's/p\\?[0-9]*$//') 2>/dev/null | head -1") or "NVMe SSD"
os_name = sh("grep -m1 PRETTY_NAME /etc/os-release | cut -d= -f2 | tr -d '\"'")
hypr = sh("hyprctl version 2>/dev/null | head -1 | cut -d' ' -f1-2")
bj["machine"] = {
    "label": f"{cpu.split('@')[0].strip()} laptop, {round(mem_kb / 1048576, 1)} GB RAM, Linux {platform.release()}",
    "cpu": f"{cpu} ({os.cpu_count()} threads)",
    "ramGb": round(mem_kb / 1048576, 1),
    "storage": disk_model.strip() or "NVMe SSD",
    "os": f"{os_name}, kernel {platform.release()}, Wayland ({hypr or 'compositor unknown'}), 1920x1080",
    "note": "The same machine for every editor, shared with other running work; editors were run one at a time.",
}
bj["measuredAt"] = datetime.date.today().isoformat()

ver = {
    "lumen": sh("lumen-text --version 2>/dev/null | head -1 | awk '{print $NF}'") or editors["lumen"].get("version"),
    "vscode": sh(f"{bench}/vscode/VSCode-linux-x64/bin/code --version 2>/dev/null | head -1") or None,
    "zed": sh("zeditor --version 2>/dev/null | awk '{print $2}'") or None,
    "sublime": ("Build " + sh("/opt/sublime_text/sublime_text --version | grep -o '[0-9]*$'")) or None,
    "openidea": (re.search(r"openidea-([\d.]+)", sh("readlink -f ~/Applications/openidea-*/AppRun") or "") or [None, None])[1],
}
for k, v in ver.items():
    if v and k in editors:
        editors[k]["version"] = v
editors["vscode"]["note"] = "Electron, fresh profile, no extensions. Windsurf and Devin are VS Code forks and are expected to land in the same range."
editors["zed"]["note"] = "Native, GPU-rendered (pacman package)"
editors["openidea"]["note"] = "Local build: a Tauri 2 app (Rust + WebKitGTK webview) shipped as an AppImage, bundles its own GTK/WebKit libraries"

# ---------------------------------------------------------------- GUI scenarios
disk = load("disk")
idle = {e: load(f"{e}-idle") for e in IDS}
big = {e: load(f"{e}-big") for e in IDS}
for e in IDS:
    i, b = idle[e], big[e]
    if i and i["stats"]["pss_mb"]:
        s = i["stats"]
        cell("ram-idle", e, s["pss_mb"]["median"],
             f"PSS summed over the whole process tree, {i['wait_s']:.0f} s after the window settled: {rng(s['pss_mb'], 0, ' MB')}. "
             f"RSS median {fm(s['rss_mb']['median'])} MB, {fm(s['procs']['median'])} processes. Peak PSS median {fm(s['pss_peak_mb']['median'])} MB.")
        cell("startup", e, s["settled_ms"]["median"],
             f"Spawn to window pixels stable with the workspace and README.md shown: {rng(s['settled_ms'], 0, ' ms')}. "
             f"Window mapped at {fm(s['mapped_ms']['median'])} ms (median).")
    if b and b["stats"]["pss_mb"]:
        s = b["stats"]
        base = idle[e]["stats"]["settled_ms"]["median"] if i else None
        cell("ram-open-100k", e, s["pss_mb"]["median"],
             f"PSS of the whole process tree {b['wait_s']:.0f} s after launching with the 100k-line file: {rng(s['pss_mb'], 0, ' MB')}; "
             f"peak median {fm(s['pss_peak_mb']['median'])} MB; RSS median {fm(s['rss_mb']['median'])} MB; {fm(s['procs']['median'])} processes (language servers included if the editor started one).")
        cell("open-100k", e, s["settled_ms"]["median"],
             f"Launch straight into the 100k-line file (startup included): {rng(s['settled_ms'], 0, ' ms')}"
             + (f"; the empty-README launch took {fm(base)} ms, so the file itself adds about {fm(s['settled_ms']['median'] - base)} ms." if base else "."))
    if disk and e in disk:
        d = disk[e]
        n = f"{d['what']}."
        if e == "lumen":
            n += f" Release binary alone {fm(d['binary_mb'], 1)} MB; the optional tsgo download adds {fm(d['tsgo_mb'])} MB."
        cell("disk", e, d["mb"], n)

# Lumen scroll/keystroke stay the test-suite numbers; the other editors cannot be measured the same way.
why = ("Lumen's figure is the CPU time of one software-rendered frame from its own test (perf.rs). The other editors expose no "
       "comparable frame-cost counter and input-to-photon latency needs a high-speed camera, so this is not comparable.")
for mid in ("scroll-frame", "keystroke"):
    for e in IDS[1:]:
        unavailable(mid, e, "not-comparable: " + why)
    metric[mid]["required"] = False

# ---------------------------------------------------------------- language servers (protocol level)
lsp = {m: load(f"lsp-{m}") for m in ("tsgo-lumen", "tsgo-full", "tsserver-vscode", "vtsls-zed")}
lab = "Language-server speed measured over the LSP protocol, not through the GUI: "
src = {"lumen": ("tsgo-lumen", "tsgo 7.0.0-dev (Lumen's narrow mirror, GOMEMLIMIT=512MiB GOGC=50)"),
       "vscode": ("tsserver-vscode", "tsserver of TypeScript 6.0.3 bundled in VS Code 1.145.2, run with VS Code's own Electron as node"),
       "zed": ("vtsls-zed", "vtsls 0.3.0 (TypeScript 5.9.3), the server Zed installs for TypeScript, run with system node")}
for e, (m, what) in src.items():
    r = lsp.get(m)
    if r and r["stats"]["ms"]:
        s = r["stats"]
        extra = ""
        if e == "lumen" and lsp.get("tsgo-full") and lsp["tsgo-full"]["stats"]["ms"]:
            f = lsp["tsgo-full"]["stats"]
            extra = f" Same tsgo on the whole 300-file project (no mirror): {fm(f['ms']['median'])} ms, {fm(f['pss_mb']['median'])} MB."
        cell("lsp-first-diagnostics", e, s["ms"]["median"],
             lab + f"{what}; spawn to the first diagnostics for src/broken.ts in the 302-file sample project: {rng(s['ms'], 0, ' ms')}." + extra)
        cell("lsp-memory", e, s["pss_mb"]["median"],
             lab + f"{what}; PSS of the server process tree 1.5 s after the diagnostics: {rng(s['pss_mb'], 0, ' MB')}, {fm(s['procs']['median'])} process(es).")
for e, why in (("sublime", "Sublime Text has no built-in TypeScript language server (it needs the separate LSP package plus a server), so the out-of-the-box editor has nothing to measure."),
               ("openidea", "No standalone TypeScript language server was identified in the install, and openidea gives no protocol endpoint to drive; not measured rather than guessed.")):
    for mid in ("lsp-first-diagnostics", "lsp-memory"):
        unavailable(mid, e, why)
metric["lsp-first-diagnostics"]["description"] = ("Open src/broken.ts of a 302-file TypeScript project and wait for the first diagnostics. "
    "Measured at the language-server protocol level: Lumen runs tsgo (the native compiler); VS Code uses its bundled tsserver; Zed uses vtsls.")
metric["lsp-memory"]["description"] = ("PSS of the TypeScript server after the first diagnostics on the same project. "
    "Lumen's lean mode loads only the open file and its imports through a narrow mirror.")
metric["ram-idle"]["description"] = ("Whole process tree (helpers, renderers, GPU) after opening a generated 302-file TypeScript workspace "
    "and README.md and letting it settle for 20 s. README.md starts no language server in any editor.")
metric["ram-open-100k"]["description"] = ("Same workspace, launched straight into a generated 100,000-line TypeScript file (about 5.8 MB), 10 s after the window settled. "
    "Includes any language server the editor starts for a .ts file.")
metric["disk"]["description"] = "Installed program on disk (MB of files, 1 MB = 2^20 bytes), without user data."
metric["startup"]["description"] = ("Process spawn until the window shows the workspace and the first file and its pixels stop changing "
    "(compositor window-mapped time in the note).")
metric["open-100k"]["description"] = "Launch straight into the 100,000-line TypeScript file until the window's pixels stop changing (startup included)."
if metric["open-100k"]["values"].get("lumen", {}).get("note"):
    metric["open-100k"]["values"]["lumen"]["note"] += " Lumen's own in-process buffer open (perf.rs) takes 21 ms."

# ---------------------------------------------------------------- CLI reference
cli = load("cli")
if cli:
    bj["facts"]["cliTypecheck"] = {
        "label": "Full type-check of the 302-file sample project (tsc --noEmit)",
        "unit": "ms",
        "source": "measured",
        "tools": [{"id": k, "version": v["version"], "command": v["command"], "median": round(v["median_ms"]), "min": round(v["min_ms"]),
                   "max": round(v["max_ms"]), "runs": v["runs"]} for k, v in cli.items()],
    }

# ---------------------------------------------------------------- methodology
bj["methodology"] = (
    "All figures were taken on one machine (see above), one editor at a time, on a freshly generated TypeScript workspace "
    "(302 files plus a 100,000-line file) with an isolated, extension-free profile per editor; each cell is the median of N runs "
    "(min and max in the tooltip) after one discarded warm-up launch. Memory is PSS (proportional set size, shared pages counted fairly) "
    "summed over the editor's whole process tree: helpers, renderers, GPU and language-server processes. Startup is the time from spawning the "
    "editor until its window shows the workspace and the first file and stops changing (screen capture polled every 100 ms), measured the same way "
    "for every editor. TypeScript timings are taken at the language-server protocol level (spawn, open a file with a type error, wait for diagnostics) "
    "with the server each editor ships. A cell marked unavailable could not be measured fairly and says why. "
    "Frame and keystroke times are only available for Lumen (CPU time of its own tests). Reproduce with tools/bench-compare.sh.")

for m in bj["metrics"]:
    for e in IDS:
        m["values"].setdefault(e, {"value": None, "source": "pending"})

# ---------------------------------------------------------------- write
os.makedirs(archive, exist_ok=True)
stamp = datetime.date.today().isoformat()
raw = {"measuredAt": stamp, "machine": bj["machine"], "versions": ver}
for f in sorted(os.listdir(out)):
    if f.endswith(".json") and not f.startswith(("t-", "l-")):
        raw[f[:-5]] = json.load(open(f"{out}/{f}"))
json.dump(raw, open(f"{archive}/results-{stamp}.json", "w"), indent=1)
json.dump(bj, open(target, "w"), indent=2, ensure_ascii=False)
open(target, "a").write("\n")
print(f"merged into {target}; raw results in {archive}/results-{stamp}.json")

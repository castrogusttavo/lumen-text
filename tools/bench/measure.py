#!/usr/bin/env python3
"""GUI measurements for tools/bench-compare.sh: startup, PSS/RSS of the whole process tree.

Runs ONE editor at a time on the live Hyprland session (windows are short-lived). Method:
  * The editor is spawned with env BENCH_TAG=<unique>; its whole process tree is every
    process whose /proc/<pid>/environ carries that tag (helpers, GPU, LSPs, daemonised children).
  * "mapped"   = ms from spawn until a window of the tree is mapped (hyprctl clients).
  * "settled"  = ms from spawn until the window's pixels stopped changing (grim at 10% scale,
                 a frame counts as a change when >1% of the pixels differ, stable for 1.5 s).
                 This is the "usable window with the file shown" time; cursor blink is below the threshold.
  * PSS/RSS    = summed over the tree (smaps_rollup) WAIT seconds after settling.
Usage: measure.py --editor lumen|vscode|zed|sublime|openidea --scenario idle|big|warm --runs N --out file.json
"""
import argparse, json, os, shutil, signal, statistics, subprocess, sys, time

HOME = os.path.expanduser("~")
BENCH = os.environ.get("LUMEN_BENCH_DIR", f"{HOME}/.cache/lumen-bench")
SAMPLE = f"{BENCH}/sample"
FILES = {"idle": f"{SAMPLE}/README.md", "big": f"{SAMPLE}/big/big.ts", "warm": f"{SAMPLE}/src/d0/mod0.ts"}
VSC = f"{BENCH}/vscode/VSCode-linux-x64"


def profile(ed):
    p = f"{BENCH}/profiles/{ed}"
    for d in ("config", "data", "state", "cache", "run"):
        os.makedirs(f"{p}/{d}", exist_ok=True)
    os.chmod(f"{p}/run", 0o700)
    return p


def editor_cmd(ed, folder, file):
    p = profile(ed)
    env = {
        "XDG_CONFIG_HOME": f"{p}/config", "XDG_DATA_HOME": f"{p}/data", "XDG_STATE_HOME": f"{p}/state",
        "XDG_CACHE_HOME": f"{p}/cache", "XDG_RUNTIME_DIR": f"{p}/run",
        # the Wayland socket lives in the real runtime dir; absolute path keeps it reachable
        "WAYLAND_DISPLAY": f"/run/user/{os.getuid()}/{os.environ.get('WAYLAND_DISPLAY', 'wayland-1')}",
    }
    if ed == "lumen":
        a = f"{HOME}/.local/share/lumen-text/current/share/lumen-text/assets"
        env.update(LUMEN_ASSETS=a, LUMEN_UPDATE_URL="http://127.0.0.1:9/latest.json")
        return [f"{HOME}/.local/bin/lumen-text", folder, file], env
    if ed == "vscode":
        env["HOME"] = f"{p}/home"  # keeps ~/.vscode-shared etc. out of the real home
        os.makedirs(env["HOME"], exist_ok=True)
        return [f"{VSC}/code", f"--user-data-dir={p}/data/code", f"--extensions-dir={p}/data/ext",
                "--disable-workspace-trust", "--skip-welcome", "--skip-release-notes", "--disable-updates",
                "--no-sandbox", "--new-window", folder, file], env
    if ed == "zed":
        return ["/usr/lib/zed/zed-editor", f"--user-data-dir={p}/data/zed", folder, file], env
    if ed == "sublime":
        return ["/opt/sublime_text/sublime_text", "-n", folder, file], env
    if ed == "openidea":
        return [f"{HOME}/Applications/openidea-0.1.33/AppRun", folder, file], env
    raise SystemExit(f"unknown editor {ed}")


def tree(tag, root=None):
    """Every process carrying BENCH_TAG in its environ, plus `root` (the spawned pid) and all
    descendants of either (some main processes rewrite their environ, so the tag alone misses them)."""
    needle = f"BENCH_TAG={tag}\0".encode()
    found, ppid = set(), {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        pid = int(d)
        try:
            st = open(f"/proc/{d}/stat").read()
            ppid[pid] = int(st[st.rindex(")") + 2:].split()[1])
            with open(f"/proc/{d}/environ", "rb") as f:
                if needle in f.read():
                    found.add(pid)
        except OSError:
            pass
    if root is not None and root in ppid:
        found.add(root)
    changed = True
    while changed:
        changed = False
        for pid, pp in ppid.items():
            if pp in found and pid not in found:
                found.add(pid)
                changed = True
    return sorted(found)


def in_tree(pid, tag, root):
    """Cheap membership test used while polling for the window."""
    for _ in range(6):
        if pid == root or has_tag(pid, tag):
            return True
        try:
            st = open(f"/proc/{pid}/stat").read()
            pid = int(st[st.rindex(")") + 2:].split()[1])
        except (OSError, ValueError):
            return False
    return False


def mem(pids):
    pss = rss = 0
    n = 0
    for p in pids:
        try:
            for ln in open(f"/proc/{p}/smaps_rollup"):
                if ln.startswith("Pss:"):
                    pss += int(ln.split()[1])
                elif ln.startswith("Rss:"):
                    rss += int(ln.split()[1])
            n += 1
        except OSError:
            pass
    return pss / 1024, rss / 1024, n


def clients():
    try:
        return json.loads(subprocess.run(["hyprctl", "clients", "-j"], capture_output=True, text=True, timeout=5).stdout)
    except Exception:
        return []


def has_tag(pid, tag):
    try:
        return f"BENCH_TAG={tag}\0".encode() in open(f"/proc/{pid}/environ", "rb").read()
    except OSError:
        return False


def shot(c):
    (x, y), (w, h) = c["at"], c["size"]
    r = subprocess.run(["grim", "-t", "ppm", "-s", "0.1", "-g", f"{x},{y} {w}x{h}", "-"], capture_output=True)
    return r.stdout


def diff_frac(a, b):
    if not a or not b or len(a) != len(b):
        return 1.0
    # compare 3-byte pixels in the payload (skip header)
    h = a.find(b"255\n") + 4
    pa, pb = a[h:], b[h:]
    n = len(pa) // 3
    d = sum(1 for i in range(0, n * 3, 3) if pa[i:i + 3] != pb[i:i + 3])
    return d / max(n, 1)


def mem_available_mb():
    for ln in open("/proc/meminfo"):
        if ln.startswith("MemAvailable"):
            return int(ln.split()[1]) / 1024


def run_once(ed, scenario, wait):
    while mem_available_mb() < 1500:
        print("  low memory, waiting...", file=sys.stderr)
        time.sleep(5)
    tag = f"{ed}-{os.getpid()}-{time.time_ns()}"
    cmd, extra = editor_cmd(ed, SAMPLE, FILES[scenario])
    env = {**os.environ, **extra, "BENCH_TAG": tag}
    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=open(f"{BENCH}/out/{ed}.stderr", "w"), start_new_session=True, cwd=BENCH)
    res = {"mapped_ms": None, "settled_ms": None}
    win = None
    try:
        while time.monotonic() - t0 < 60:
            for c in clients():
                if c.get("mapped") and in_tree(c["pid"], tag, proc.pid):
                    win = c
                    break
            if win:
                res["mapped_ms"] = (time.monotonic() - t0) * 1000
                break
            time.sleep(0.02)
        if win is None:
            res["error"] = "no window within 60 s"
            return res
        # visual settle
        last_change = time.monotonic()
        prev = shot(win)
        while time.monotonic() - t0 < 60:
            time.sleep(0.1)
            cur = [c for c in clients() if c["address"] == win["address"]]
            if cur:
                win = cur[0]
            now = shot(win)
            if diff_frac(prev, now) > 0.01:
                last_change = time.monotonic()
            prev = now
            if time.monotonic() - last_change >= 1.5:
                break
        res["settled_ms"] = (last_change - t0) * 1000
        t_end = t0 + res["settled_ms"] / 1000 + wait
        peak = 0.0
        while time.monotonic() < t_end:
            time.sleep(min(2.0, max(0.0, t_end - time.monotonic())))
            p, r, n = mem(tree(tag, proc.pid))
            peak = max(peak, p)
        pids = tree(tag, proc.pid)
        p, r, n = mem(pids)
        res.update(pss_mb=p, rss_mb=r, procs=n, pss_peak_mb=max(peak, p))
        top = []
        for q in pids:
            try:
                comm = open(f"/proc/{q}/comm").read().strip()
                top.append([comm, round(mem([q])[0], 1)])
            except OSError:
                pass
        res["top"] = sorted(top, key=lambda t: -t[1])[:4]
        return res
    finally:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except OSError:
            pass
        for _ in range(60):
            if not tree(tag, proc.pid):
                break
            time.sleep(0.1)
        left = tree(tag, proc.pid)
        for p in left:
            try:
                os.kill(p, signal.SIGKILL)
            except OSError:
                pass
        if left:
            res["killed_leftovers"] = len(left)
        time.sleep(1.0)


def stats(vals):
    v = [x for x in vals if x is not None]
    if not v:
        return None
    return {"median": statistics.median(v), "min": min(v), "max": max(v), "n": len(v)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--editor", required=True)
    ap.add_argument("--scenario", choices=["idle", "big", "warm"], required=True)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--wait", type=float, default=None, help="seconds to settle before reading memory")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    wait = a.wait if a.wait is not None else {"idle": 20, "big": 10, "warm": 45}[a.scenario]
    os.makedirs(f"{BENCH}/out", exist_ok=True)
    runs = []
    for i in range(1 if a.scenario == "warm" else a.runs):
        print(f"[{a.editor}/{a.scenario}] run {i + 1}", file=sys.stderr)
        r = run_once(a.editor, a.scenario, wait)
        print("   ", {k: (round(v, 1) if isinstance(v, float) else v) for k, v in r.items()}, file=sys.stderr)
        runs.append(r)
    out = {"editor": a.editor, "scenario": a.scenario, "wait_s": wait, "runs": runs,
           "stats": {k: stats([r.get(k) for r in runs if "error" not in r]) for k in
                     ("mapped_ms", "settled_ms", "pss_mb", "rss_mb", "procs", "pss_peak_mb")}}
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    json.dump(out, open(a.out, "w"), indent=2)


main()

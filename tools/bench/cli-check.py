#!/usr/bin/env python3
"""CLI reference: full type-check of the sample project with tsc 6 (JS), tsc 7 (native) and tsgo.
Usage: cli-check.py <bench dir> <runs> <out.json>"""
import json, os, statistics, subprocess, sys, time
bench, runs, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
home = os.path.expanduser("~")
sample = f"{bench}/sample"
tools = {
    "tsc-6.0.3-js": [ "node", f"{bench}/ts6/node_modules/typescript/bin/tsc", "-p", sample, "--noEmit"],
    "tsc-7.0.2-native": [f"{bench}/ts/node_modules/.bin/tsc", "-p", sample, "--noEmit"],
    "tsgo-lumen-bundled": [f"{home}/.local/share/lumen-text/tsgo/node_modules/@typescript/native-preview-linux-x64/lib/tsgo", "-p", sample, "--noEmit"],
}
res = {}
for name, cmd in tools.items():
    def once():
        t = time.monotonic()
        r = subprocess.run(cmd, capture_output=True, text=True)
        return (time.monotonic() - t) * 1000, r.stdout.count("error TS")
    once()  # warm-up
    ts = [once() for _ in range(runs)]
    v = subprocess.run(cmd[:-4] + ["--version"], capture_output=True, text=True).stdout.strip()
    ms = [t for t, _ in ts]
    res[name] = {"command": " ".join(c.replace(home, "~") for c in cmd), "version": v, "errors_reported": ts[0][1],
                 "median_ms": statistics.median(ms), "min_ms": min(ms), "max_ms": max(ms), "runs": runs}
    print(name, res[name], file=sys.stderr)
json.dump(res, open(out, "w"), indent=2)

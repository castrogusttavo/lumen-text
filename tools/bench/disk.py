#!/usr/bin/env python3
"""Installed size on disk (MB, 1 MB = 1048576 bytes, apparent file sizes summed, symlinks not followed).
Usage: disk.py <bench dir> <out.json>"""
import json, os, subprocess, sys
bench, out = sys.argv[1], sys.argv[2]
home = os.path.expanduser("~")

def dir_mb(p):
    t = 0
    for r, _, fs in os.walk(p):
        for f in fs:
            fp = os.path.join(r, f)
            if not os.path.islink(fp):
                t += os.path.getsize(fp)
    return t / 1048576

def pkg_mb(pkg):
    files = [l.split(" ", 1)[1] for l in subprocess.run(["pacman", "-Ql", pkg], capture_output=True, text=True).stdout.splitlines()]
    return sum(os.path.getsize(f) for f in files if os.path.isfile(f) and not os.path.islink(f)) / 1048576

lv = os.path.realpath(f"{home}/.local/share/lumen-text/current")
r = {
    "lumen": {"mb": dir_mb(lv), "what": f"{lv.replace(home, '~')} (binary + assets)", "binary_mb": os.path.getsize(f"{lv}/bin/lumen-text") / 1048576,
              "tsgo_mb": dir_mb(f"{home}/.local/share/lumen-text/tsgo")},
    "vscode": {"mb": dir_mb(f"{bench}/vscode/VSCode-linux-x64"), "what": "official linux-x64 stable tarball, extracted"},
    "zed": {"mb": pkg_mb("zed"), "what": "files of the pacman package zed"},
    "sublime": {"mb": dir_mb("/opt/sublime_text"), "what": "/opt/sublime_text"},
    "openidea": {"mb": dir_mb(f"{home}/Applications/openidea-0.1.33"), "what": "~/Applications/openidea-0.1.33 (extracted AppImage, bundles WebKitGTK/GTK libs)"},
}
json.dump(r, open(out, "w"), indent=2)
print(json.dumps({k: round(v["mb"], 1) for k, v in r.items()}), file=sys.stderr)

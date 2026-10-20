#!/usr/bin/env python3
"""Run a built mobile app in an isolated X11 desktop with a window manager.

Usage: xvfb-run -a sh -c 'openbox & python3 mobile/check-layout.py /path/to/openfader-mobile'
Requires xdotool and ImageMagick. Screenshots remain in the printed directory.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
root = Path(tempfile.mkdtemp(prefix="openfader-layout-"))
music = root / "Music"
music.mkdir()
env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(root / "config"),
           XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
           SLINT_BACKEND="winit-software", WINIT_X11_SCALE_FACTOR="1")
with (root / "runtime.log").open("w") as log:
    app = subprocess.Popen([binary, str(music)], env=env, stdout=log, stderr=log)
    try:
        window = subprocess.check_output(
            ["xdotool", "search", "--sync", "--onlyvisible", "--pid", str(app.pid)],
            timeout=30, text=True).splitlines()[0]
        for width, height in [(320, 568), (375, 667), (414, 896), (540, 1200),
                              (640, 360), (768, 600), (1000, 540), (320, 568)]:
            subprocess.run(["xdotool", "windowsize", window, str(width), str(height)], check=True)
            time.sleep(0.3)
            geometry = subprocess.check_output(
                ["xdotool", "getwindowgeometry", "--shell", window], text=True)
            values = dict(line.split("=", 1) for line in geometry.splitlines() if "=" in line)
            assert (int(values["WIDTH"]), int(values["HEIGHT"])) == (width, height), geometry
            assert app.poll() is None, (root / "runtime.log").read_text()
            subprocess.run(["import", "-window", window, str(root / f"{width}x{height}.png")], check=True)
            print(f"PASS {width}x{height}", flush=True)
    finally:
        app.terminate()
        app.wait(timeout=10)
assert "panicked" not in (root / "runtime.log").read_text()
print(f"Screenshots and runtime log: {root}")

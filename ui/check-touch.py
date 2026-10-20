#!/usr/bin/env python3
"""Exercise the full desktop binary's Pocket theme in an isolated X11/Openbox session.

xvfb-run -a -s '-screen 0 1440x1400x24' sh -c 'openbox & python3 ui/check-touch.py target/release/openfader [2|4]'
Requires xdotool and ImageMagick; leaves screenshots and logs in the printed temporary directory.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
root = Path(tempfile.mkdtemp(prefix="openfader-touch-"))
decks = int(sys.argv[2]) if len(sys.argv) > 2 else 2
assert decks in (2, 4), "Expected two or four decks"
(root / "Music").mkdir()
config = root / "config/openfader"
config.mkdir(parents=True)
(config / "settings.conf").write_text(f"skin=Pocket\nwelcomed=true\nline_space=large\nbrowse_font=large\ndecks={decks}\n")
(root / ".asoundrc").write_text("pcm.!default { type null }\n")
env = dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(root / "config"),
           XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
           OPENFADER_SKINS=str(Path(__file__).resolve().parent.parent / "extension/skin"),
           SLINT_BACKEND="winit-software", WINIT_X11_SCALE_FACTOR="1")
with (root / "runtime.log").open("w") as log:
    app = subprocess.Popen([binary, str(root / "Music")], env=env, stdout=log, stderr=log)
    try:
        window = subprocess.check_output(
            ["xdotool", "search", "--sync", "--onlyvisible", "--pid", str(app.pid)],
            timeout=45, text=True).splitlines()[0]
        for width, height in [(320, 568), (375, 667), (414, 896), (540, 1200),
                              (615, 360), (616, 360), (640, 360), (1200, 540), (768, 600), (320, 568)]:
            subprocess.run(["xdotool", "windowsize", window, str(width), str(height)], check=True)
            time.sleep(0.4)
            geometry = subprocess.check_output(["xdotool", "getwindowgeometry", "--shell", window], text=True)
            values = dict(line.split("=", 1) for line in geometry.splitlines() if "=" in line)
            assert (int(values["WIDTH"]), int(values["HEIGHT"])) == (width, height), geometry
            assert app.poll() is None, (root / "runtime.log").read_text()
            subprocess.run(["import", "-window", window, str(root / f"{width}x{height}.png")], check=True)
            print(f"PASS {width}x{height}", flush=True)
    finally:
        app.terminate()
        app.wait(timeout=10)
text = (root / "runtime.log").read_text()
assert "panicked" not in text and "Binding loop" not in text, text
print(f"Screenshots and runtime log: {root}")

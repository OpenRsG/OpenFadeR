#!/usr/bin/env bash
# Smoke test of a built app on one OS: start it with a private home and settings, let it run, take a screenshot of the
# desktop, check that the process is still alive and that its output holds no panic, then close it.
# Usage: smoke.sh <openfader binary> <output folder> [seconds to wait, default 20]
# Writes <output folder>/screenshot.png and app.log. Linux needs a display (run it under xvfb-run) and ImageMagick.
set -u
bin=$1
out=$2
wait_s=${3:-20}
mkdir -p "$out"
out=$(cd "$out" && pwd)
[ -f "$bin.exe" ] && bin="$bin.exe"
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")

case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN*) os=windows ;;
Darwin) os=macos ;;
*) os=linux ;;
esac

# Everything the app writes (settings, store, caches, its music folder) goes below this private home.
home=$(mktemp -d)
export HOME="$home"
if [ "$os" = windows ]; then
	w=$(cygpath -w "$home")
	mkdir -p "$home/AppData/Roaming" "$home/AppData/Local"
	export USERPROFILE="$w" APPDATA="$w\\AppData\\Roaming" LOCALAPPDATA="$w\\AppData\\Local"
fi

"$bin" >"$out/app.log" 2>&1 &
pid=$!
sleep "$wait_s"

case "$os" in
windows)
	shot=$(cygpath -w "$out/screenshot.png")
	powershell -NoProfile -Command "
		Add-Type -AssemblyName System.Windows.Forms, System.Drawing
		\$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
		\$bmp = New-Object System.Drawing.Bitmap \$b.Width, \$b.Height
		\$g = [System.Drawing.Graphics]::FromImage(\$bmp)
		\$g.CopyFromScreen(\$b.Left, \$b.Top, 0, 0, \$bmp.Size)
		\$bmp.Save('$shot')" || echo "screenshot failed"
	;;
macos) screencapture -x "$out/screenshot.png" || echo "screenshot failed" ;;
*) import -window root "$out/screenshot.png" || echo "screenshot failed" ;;
esac

status=0
if kill -0 "$pid" 2>/dev/null; then
	echo "alive after ${wait_s}s"
else
	echo "::error::the app is no longer running after ${wait_s}s"
	status=1
fi

# Close it: politely, then for good (on Windows through the native process id).
if [ "$os" = windows ] && [ -r "/proc/$pid/winpid" ]; then
	taskkill //PID "$(cat "/proc/$pid/winpid")" //F //T >/dev/null 2>&1
else
	kill "$pid" 2>/dev/null
	sleep 2
	kill -9 "$pid" 2>/dev/null
fi
wait "$pid" 2>/dev/null

echo "---- app output ----"
tail -n 60 "$out/app.log"
echo "--------------------"
if grep -qiE "panicked at|RUST_BACKTRACE|stack overflow" "$out/app.log"; then
	echo "::error::the app panicked (see app.log)"
	status=1
fi
# The plugins checkout (or the release archive's extension/plugin) must have been found next to the executable.
if ! grep -qE "^plugins: [1-9][0-9]* found" "$out/app.log"; then
	echo "::error::no plugin was found"
	status=1
fi
if [ ! -s "$out/screenshot.png" ]; then
	echo "::error::no screenshot was taken"
	status=1
fi
exit $status

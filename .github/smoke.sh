#!/usr/bin/env bash
# Smoke test of a built app on one OS: start it with a private home and settings, let it run, take a screenshot of the
# desktop, check that the process is still alive and that its output holds no panic, then close it.
# Usage: smoke.sh <openfader binary> <output folder> [seconds to wait, default 20]
# Writes <output folder>/screenshot.png and app.log. Linux needs a display (run it under xvfb-run) and ImageMagick.
# Start-up time: with SMOKE_TRACKS=<n> and SMOKE_GEN=<the big_library example binary> the private store is first filled with
# n synthetic tracks (`cargo build --release --example big_library`), and on Linux the app's `startup: window shown after
# <ms> ms` line must show at most SMOKE_STARTUP_MS (default 5000).
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

if [ -n "${SMOKE_TRACKS:-}" ]; then
	"$SMOKE_GEN" "$SMOKE_TRACKS" || {
		echo "::error::could not make the test library"
		exit 1
	}
fi

(
	# Catch oversized UI constructors on Linux too, using the Windows executable's default stack budget.
	if [ "$os" = linux ]; then
		ulimit -s 1024
	fi
	exec "$bin"
) >"$out/app.log" 2>&1 &
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
# Start-up: the window must show within the limit, even with a big library (the library may fill in after it).
if [ -n "${SMOKE_TRACKS:-}" ] && [ "$os" = linux ]; then
	ms=$(sed -n 's/^startup: window shown after \([0-9]*\) ms.*/\1/p' "$out/app.log" | head -n 1)
	if [ -z "$ms" ]; then
		echo "::error::no 'startup: window shown' line in the output"
		status=1
	elif [ "$ms" -gt "${SMOKE_STARTUP_MS:-5000}" ]; then
		echo "::error::the window took ${ms} ms to show with $SMOKE_TRACKS tracks (limit ${SMOKE_STARTUP_MS:-5000} ms)"
		status=1
	else
		echo "start-up: window shown after ${ms} ms with $SMOKE_TRACKS tracks"
	fi
fi
if [ ! -s "$out/screenshot.png" ]; then
	echo "::error::no screenshot was taken"
	status=1
fi
exit $status

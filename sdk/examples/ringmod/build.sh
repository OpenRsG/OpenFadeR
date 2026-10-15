#!/bin/sh
# Builds libringmod.so next to plugin.json: the folder is then a plugin ready to copy into the plugins folder.
set -e
cd "$(dirname "$0")"
cc -O2 -shared -fPIC -I../.. ringmod.c -o libringmod.so -lm

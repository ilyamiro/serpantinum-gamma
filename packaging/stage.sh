#!/bin/sh
# Lay out an installed serpantinum-gamma under $1 with the prefix /usr, from a release build.
# The release tarball is made from this tree.
set -eu
dest=${1:?usage: stage.sh DEST}
cd "$(dirname "$0")/.."

install -Dm755 target/release/serpantinum-gamma "$dest/usr/bin/serpantinum-gamma"
install -Dm644 assets/rs.wl-gammarelay.service "$dest/usr/share/dbus-1/services/rs.wl-gammarelay.service"
sed -i 's|@out@|/usr|' "$dest/usr/share/dbus-1/services/rs.wl-gammarelay.service"
install -Dm644 LICENSE "$dest/usr/share/licenses/serpantinum-gamma/LICENSE"

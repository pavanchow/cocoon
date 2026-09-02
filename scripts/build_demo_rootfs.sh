#!/bin/sh
# Build a minimal busybox rootfs and a bundle, for proving isolation.
set -e
B="${1:-/storage/cocoon-build/demo}"
rm -rf "$B"
mkdir -p "$B/rootfs/bin" "$B/rootfs/proc"
cp "$(command -v busybox)" "$B/rootfs/bin/busybox"
for a in sh hostname echo id ls cat env ps grep wc true; do ln -sf busybox "$B/rootfs/bin/$a"; done
# copy the dynamic loader and every shared library busybox needs, preserving paths
for lib in $(ldd "$(command -v busybox)" | grep -oE '/[^ ]+\.so[.0-9]*'); do
  mkdir -p "$B/rootfs$(dirname "$lib")"
  cp "$lib" "$B/rootfs$lib"
done
cat > "$B/cocoon.conf" <<'CONF'
hostname = cocoonbox
cwd      = /
argv     = /bin/busybox sh -c "echo host=$(hostname) pid=$$ uid=$(id -u) procs=$(ls -d /proc/[0-9]* | wc -l)"
env      = PATH=/bin
CONF
echo "bundle at $B"

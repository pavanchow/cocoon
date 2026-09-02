#!/bin/sh
# Prove cgroup memory enforcement. Run this INSIDE a delegated cgroup, e.g.:
#   systemd-run --user --scope -p Delegate=yes -- sh scripts/cgroup_proof.sh
# It runs a container capped at 8 MiB whose payload tries to allocate far more.
# With enforcement the payload is OOM-killed before printing SURVIVED.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
H="${TMPDIR:-/tmp}/cocoon-cgproof"
sh "$here/build_demo_rootfs.sh" "$H" >/dev/null 2>&1
cat > "$H/cocoon.conf" <<'CONF'
hostname   = memtest
cwd        = /
argv       = /bin/busybox sh -c "s=x; i=0; while [ $i -lt 25 ]; do s=$s$s; i=$((i+1)); done; echo SURVIVED"
memory_max = 8388608
env        = PATH=/bin
CONF
bin="$root/target/debug/cocoon"
[ -x "$bin" ] || bin="$root/target/release/cocoon"
out="$("$bin" run "$H" 2>&1)"; code=$?
echo "container output: [$out]  exit=$code"
if echo "$out" | grep -q SURVIVED; then
  echo "FAIL: payload survived, memory_max was not enforced"; exit 1
else
  echo "PROOF OK: payload was killed under the 8 MiB memory_max (cgroup enforced)"
fi

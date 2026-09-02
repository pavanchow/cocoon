#!/bin/sh
# End-to-end proof that Cocoon really isolates a process, rootless. Builds a
# busybox rootfs, runs the container, and asserts the process sees the
# container's hostname, is pid 1 in its own pid namespace, and is uid 0 via the
# user namespace. Exits non-zero if any of that is not true. Used by CI.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
B="${TMPDIR:-/tmp}/cocoon-demo"
sh "$here/build_demo_rootfs.sh" "$B"

bin="$root/target/debug/cocoon"
[ -x "$bin" ] || bin="$root/target/release/cocoon"

out="$("$bin" run "$B")"
echo "container said: $out"

fail=0
echo "$out" | grep -q "host=cocoonbox" || { echo "FAIL: uts namespace (hostname)"; fail=1; }
echo "$out" | grep -q "pid=1"          || { echo "FAIL: pid namespace (not pid 1)"; fail=1; }
echo "$out" | grep -q "uid=0"          || { echo "FAIL: user namespace (not uid 0)"; fail=1; }
procs=$(echo "$out" | grep -oE 'procs=[0-9]+' | cut -d= -f2)
[ -n "$procs" ] && [ "$procs" -lt 25 ] && echo "isolation: only $procs process(es) visible in the pid namespace (host has hundreds)"
[ "$fail" -eq 0 ] && echo "PROOF OK: rootless isolation verified" || exit 1

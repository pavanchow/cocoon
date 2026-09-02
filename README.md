# Cocoon

**A small rootless Linux container runtime, built from scratch in Rust.** It isolates a process with user, mount, pid, uts, and ipc namespaces and `pivot_root`s into a bundle's root filesystem, all without root. Readable end to end. By **Pavan Nallamothu** ([`pavanchow`](https://github.com/pavanchow)).

Container runtimes look like magic and read like a mess. Cocoon is the opposite: the entire isolation sequence is one short, plain file you can read in a sitting, and it runs **rootless**, so you can create a real container as an ordinary user.

- **Why use it.** To see exactly how a container is made: `unshare` namespaces, map your uid to root inside a user namespace, `fork` to become pid 1, `pivot_root` into a rootfs, mount a fresh `/proc`, and `exec`. No daemon, no thousand-line runtime, no root.
- **What is different.** The decision and the execution are split. A [`Plan`](src/plan.rs) is computed from the bundle config with no side effects, so the whole decision (which namespaces, what to run, which limits) is unit-tested on any OS, including macOS. Only the final step touches Linux syscalls.

## Quickstart

```sh
cargo build
cargo test                          # 14 tests: config, plan, and lifecycle (run anywhere)

./target/debug/cocoon spec mybox    # write a bundle skeleton (cocoon.conf + rootfs/)
# populate mybox/rootfs with a root filesystem (see scripts/build_demo_rootfs.sh)
./target/debug/cocoon plan mybox    # inspect what running it will do (works on any OS)
./target/debug/cocoon run  mybox    # run the container (Linux only)
```

## It really isolates (rootless)

`scripts/prove.sh` builds a busybox rootfs, runs a container, and checks what the process inside sees. On a normal Linux user account:

```
container said: host=cocoonbox pid=1 uid=0 procs=4
isolation: only 4 process(es) visible in the pid namespace (host has hundreds)
PROOF OK: rootless isolation verified
```

The host is `kali` at uid `1000`. Inside, the process has the container's hostname (uts namespace), is pid 1 (pid namespace), is uid 0 (user namespace, mapped from your real uid, no privilege used), and can see only its own processes. This runs in CI on every push.

## The isolation sequence

The whole runtime is [`src/exec_linux.rs`](src/exec_linux.rs):

1. `unshare` new user, mount, pid, uts, ipc (and optionally net) namespaces.
2. Map our outer uid/gid to 0 inside the user namespace. This is what lets the rest work without root.
3. `fork`. The child is pid 1 in the new pid namespace.
4. In the child: set the hostname, make the mount tree private, `pivot_root` into the bundle rootfs, mount a fresh `/proc`, bind a minimal `/dev`, drop the old root, optionally remount the rootfs read-only, drop capabilities and set `no_new_privs`, install a seccomp filter, then `exec` the process.
5. The parent waits and returns the exit code.

## The bundle

A bundle is a directory with a `rootfs/` subdirectory and a `cocoon.conf`:

```
hostname = cocoonbox
cwd      = /
argv     = /bin/busybox sh -c "hostname; echo pid=$$"
env      = PATH=/bin
# net        = isolated     # isolate the network namespace (no connectivity without setup)
# memory_max = 67108864     # parsed into the plan; enforcement is future work
# pids_max   = 64
```

## Hardening

Before `exec`, the container process is deprivileged: `no_new_privs` is set, the
ambient capability set is cleared, and the whole capability bounding set is
dropped, so it cannot gain privileges. A small seccomp filter denies a curated
set of dangerous syscalls (`keyctl`, `ptrace`, `mount`, `bpf`, module loading,
and more) with `EPERM`. A minimal `/dev` (null, zero, full, random, urandom,
tty) is bound in, and `readonly = true` remounts the rootfs read-only. All of
this is verified inside a running container by `scripts/prove.sh` and the tests.

## Limitations, honestly

Rootless and minimal on purpose. There is no image format or layering (bring your
own rootfs directory), and no network setup for the isolated net namespace. The
seccomp filter is a small curated deny-list, not Docker's full profile. Cgroup
`memory_max` and `pids_max` are applied only when cocoon runs inside a properly
delegated cgroup v2; rootless delegation is environment-dependent, and where it
is unavailable cocoon prints a warning and runs without enforcement rather than
pretending. The point is a readable, real isolation core, not a drop-in for
runc. See [DESIGN.md](DESIGN.md).

## License

MIT.

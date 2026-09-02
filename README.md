# Cocoon

**A safe code-execution sandbox for AI agents, and a rootless Linux container runtime you can read end to end.** It runs a command in its own user, mount, pid, uts, and ipc namespaces, `pivot_root`s into a root filesystem, drops all capabilities, sets `no_new_privs`, and installs a seccomp filter, then hands back a machine-readable result with the exit code, captured output, wall time, and peak memory. All rootless, no daemon, one small binary. By **Pavan Nallamothu** ([`pavanchow`](https://github.com/pavanchow)).

## Why this exists

An AI agent that writes and runs code needs to run that code somewhere it cannot do harm: isolated, resource-limited, killed if it hangs, and returning a result the agent can parse. The existing options do not fit that shape.

- **Docker** needs a daemon and a root-ish setup, ships images, and is heavy to embed.
- **runc / bubblewrap** are bare isolation CLIs: they give you a namespace but no timeout, no metering, and no structured result. You script that yourself.
- **gVisor / Firecracker / a VM** are strong but heavyweight, and are a lot to stand up just to run `python -c ...` and read the output.

Cocoon fills the small gap in the middle: give it a command, get back a JSON [`Outcome`](src/lib.rs). It is rootless (needs no `sudo` and runs in CI), has no daemon and no image format, is one static-ish binary you can drop next to your agent, and it speaks the **Model Context Protocol**, so Claude and other agents can call it as a tool directly.

```jsonc
// cocoon exec --json ./bundle
{"exit_code":0,"timed_out":false,"oom_killed":false,"wall_ms":6,"peak_mem_kib":2776,
 "stdout":"hello from the sandbox\n","stderr":""}
```

And because the whole isolation sequence is one short, plain file, you can read exactly how the sandbox is built rather than trusting a black box.

## Quickstart

```sh
cargo build
cargo test                          # config, plan, lifecycle, and (on Linux) real containers

./target/debug/cocoon spec mybox    # write a bundle skeleton (cocoon.conf + rootfs/)
# populate mybox/rootfs with a root filesystem (see scripts/build_demo_rootfs.sh)
./target/debug/cocoon plan mybox    # inspect what running it will do (works on any OS)
./target/debug/cocoon run  mybox    # run the container, forward its exit code (Linux only)
./target/debug/cocoon exec  mybox --json   # run it and print the measured Outcome
./target/debug/cocoon mcp           # serve the sandbox to agents over MCP on stdio
```

## The measured result

`cocoon exec` runs the bundle in the sandbox and reports an [`Outcome`](src/lib.rs): `exit_code`, `stdout`, `stderr`, `wall_ms`, `peak_mem_kib`, and the `timed_out` / `oom_killed` flags. Output is captured through pipes drained as the process runs, so a chatty program never deadlocks. A `--timeout` (or a `timeout` config key like `5s`, `500ms`, `2m`) kills a run that overruns and reports `timed_out: true`. `--json` prints the whole thing on one line for a caller to parse.

## For agents: the MCP server

`cocoon mcp` speaks JSON-RPC 2.0 over stdio and exposes one tool, `run_in_sandbox`. Point an MCP client at it and an agent can run code safely:

```jsonc
// tools/call
{"name":"run_in_sandbox","arguments":{
  "command":"echo hi; python3 -c 'print(2**10)'",   // argv array or a shell string
  "timeout":"10s",
  "workdir":"/home/me/scratch"                        // bound writable at /work
}}
// result text is the Outcome JSON
```

`command` is either an argv array or a shell string. `timeout`, `memory_max`, and `workdir` are optional. With no `rootfs` given, Cocoon builds a minimal busybox root filesystem once under the user cache directory and reuses it.

## Profiles

`profile = strict` or `profile = build` sets sensible defaults; any explicit key still wins.

- **strict**: network off, read-only base, 5s timeout, 128 MiB. For running untrusted code.
- **build**: writable base, network off, 5m timeout, 1 GiB. For compiling and generating artifacts.

## The bundle

A bundle is a directory with a `rootfs/` subdirectory and a `cocoon.conf`:

```
profile  = strict                 # optional: strict | build (defaults, still overridable)
hostname = cocoonbox
cwd      = /
argv     = /bin/busybox sh -c "hostname; echo pid=$$"
env      = PATH=/bin
timeout  = 5s                     # 5s, 500ms, 2m, or a bare number of seconds
mount    = /host/data:/work:rw    # bind a host dir in (ro or rw); may repeat
# net        = isolated           # isolate the network namespace (no connectivity without setup)
# readonly   = true               # remount the base rootfs read-only
# memory_max = 67108864           # best-effort; enforced where cgroup v2 is delegated
# pids_max   = 64
```

## It really isolates (rootless)

`scripts/prove.sh` builds a busybox rootfs, runs a container, and checks what the process inside sees. On a normal Linux user account:

```
container said: host=cocoonbox pid=1 uid=0 procs=4
isolation: only 4 process(es) visible in the pid namespace (host has hundreds)
PROOF OK: rootless isolation verified
```

Inside, the process has the container's hostname (uts namespace), is pid 1 (pid namespace), is uid 0 (user namespace, mapped from your real uid, no privilege used), and sees only its own processes. This runs in CI on every push.

## The isolation sequence

The whole runtime is [`src/exec_linux.rs`](src/exec_linux.rs):

1. `unshare` new user, mount, pid, uts, ipc (and optionally net) namespaces.
2. Map our outer uid/gid to 0 inside the user namespace. This is what lets the rest work without root.
3. `fork`. The child is pid 1 in the new pid namespace.
4. In the child: set the hostname, make the mount tree private, `pivot_root` into the bundle rootfs, mount a fresh `/proc`, bind a minimal `/dev`, bind any requested host mounts, drop the old root, optionally remount the rootfs read-only, drop capabilities and set `no_new_privs`, install a seccomp filter, then `exec` the process.
5. The parent captures output, enforces the timeout, waits, and returns the measured result.

## Hardening

Before `exec`, the process is deprivileged: `no_new_privs` is set, the ambient capability set is cleared, and the whole capability bounding set is dropped, so the executed code runs with an empty capability set (`CapEff` is `0`) and cannot regain privilege. A seccomp filter then denies a curated set of dangerous syscalls with `EPERM`, including the whole mount family (`mount`, `umount2`, `mount_setattr`, `move_mount`, `open_tree`, the `fs*` calls, `pivot_root`, `chroot`), so a read-only mount cannot be re-opened from inside. A minimal `/dev` is bound in, and `readonly = true` remounts the base rootfs read-only. All of this is verified inside a running container by `scripts/prove.sh` and the tests.

## Limitations, honestly

Rootless and minimal on purpose. There is no image format or layering (bring your own rootfs directory, or let `cocoon mcp` build a busybox one), and no network setup for the isolated net namespace. The seccomp filter is a curated deny-list, not Docker's full profile. Cgroup `memory_max` and `pids_max` are applied only when cocoon runs inside a properly delegated cgroup v2; rootless delegation is environment-dependent, and where it is unavailable cocoon prints a warning and runs without enforcement rather than pretending. The point is a readable, real isolation core with a usable agent interface, not a drop-in for runc. See [DESIGN.md](DESIGN.md).

## License

MIT.

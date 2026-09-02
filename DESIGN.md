# Design

Cocoon is a safe code-execution sandbox for AI agents, built on a container runtime you can read. The job is narrow: take a command, run it isolated and deprivileged and rootless, bound it in time and memory, and return a machine-readable result. Every choice serves two things at once, a tiny readable surface and a usable agent interface, with a hard split between deciding what to do and doing it.

The whole runtime is small enough to audit in a sitting, which matters when the thing you are trusting is what stands between your host and code an agent just generated.

## Decide, then execute

The runtime is two halves:

- **Decide** (`config.rs`, `plan.rs`, `state.rs`): parse the bundle config, compute a `Plan`, model the lifecycle. Pure data, no syscalls. This half compiles and is unit-tested on any OS, so the interesting logic (which namespaces, argv splitting with quotes, validation, the state machine) is covered without a Linux box.
- **Execute** (`exec_linux.rs`): take a `Plan` and a rootfs and make an isolated process. This half is Linux-only and behind `#[cfg(target_os = "linux")]`, so a macOS build still compiles and tests the decide half. On other systems `run` returns an `Unsupported` error and `cocoon plan` still works.

The executor never re-reads the config. It consumes a `Plan`. That is what makes the decision testable in isolation and the execution a straight line.

## Rootless by user namespace

Cocoon uses a user namespace and maps the caller's uid/gid to 0 inside it. Inside that namespace the process is "root" and can do the privileged-looking operations (sethostname, mount, pivot_root) that a container needs, while on the host it is still just your unprivileged user. This is why Cocoon needs no `sudo` and runs in CI. The one host requirement is that unprivileged user namespaces are enabled (`kernel.unprivileged_userns_clone`, on by default on modern distros).

`setgroups` must be written `deny` before the gid map is allowed for a rootless mapping; the code does this in order.

## Why fork after unshare

`unshare(CLONE_NEWPID)` does not move the calling process into the new pid namespace; it arranges that its next child is pid 1 there. So the sequence is unshare, set up the uid/gid maps, then `fork`. The child does the hostname, mount, and `pivot_root` work and becomes the container's init; the parent waits and reports the exit code.

## pivot_root, not chroot

`pivot_root` swaps the process's root for the bundle rootfs and lets the old root be unmounted and dropped, so the container cannot walk back out the way a `chroot` can be escaped. It requires the new root to be a mount point, so the code bind-mounts the rootfs onto itself first, then pivots, mounts a fresh `/proc` (so pids reflect the new namespace), detaches the old root, and `exec`s.

## Deprivileging

Isolation keeps the container in its own namespaces; deprivileging keeps it from
climbing back out. After all the mount work, and only then, the child sets
`no_new_privs`, clears the ambient capabilities, and drops the entire capability
bounding set, so neither it nor anything it execs can regain privilege. Last, it
installs a seccomp filter that denies a curated set of dangerous syscalls with
`EPERM`. Order matters: the mounts need capabilities, so deprivileging is the
final step before `exec`, and `no_new_privs` must precede the seccomp load for an
unprivileged process to be allowed to install it.

## Cgroups, honestly

`memory_max` and `pids_max` are wired to cgroup v2 with the standard leaf-manager
dance (move the runtime into a sibling cgroup so the parent can enable
controllers, then create a limited child for the container). This works only when
the runtime has a writable, delegated cgroup subtree. Rootless delegation is
environment-dependent: on a system without it, even `systemd-run --user` leaves
the scope root owned by systemd, so the controller cannot be enabled. Cocoon
therefore treats limits as best effort. It enforces them where it can, and where
it cannot it prints a warning and runs without them rather than silently
claiming a limit it did not set. Full rootless enforcement would need systemd
dbus integration, which is deliberately out of scope.

## Measured execution

`cocoon run` forwards the child's exit code and is done. `cocoon exec` is the agent path: it captures stdout and stderr, times the run, records peak memory, enforces a timeout, and returns a structured `Outcome`. The child's stdout and stderr are redirected into pipes; the parent drains them with a non-blocking `poll` loop in the same thread while it also polls `waitpid` and the deadline. Draining as data arrives (rather than reading after exit) is what keeps a chatty program from deadlocking on a full pipe. The loop cannot use reader threads: `unshare(CLONE_NEWPID)` makes the kernel refuse to create new threads in the parent (a thread cannot cross the pid-namespace boundary), so a threaded drain fails `EINVAL`. On the deadline the parent `SIGKILL`s the child and marks `timed_out`. Peak memory comes from `getrusage(RUSAGE_CHILDREN)`; OOM is read from the cgroup `memory.events` where a cgroup is in play.

## Filesystem policy

A `mount = SOURCE:TARGET:ro|rw` line binds a host directory into the sandbox, so an agent can hand in a writable `/work` and read results back while the base stays read-only. The bind happens in the child after `pivot_root` but before the old root is detached, because the host source is only reachable then (under `/.oldroot`). A read-only mount is bound and then remounted read-only, since a bind cannot be made read-only in one step. The base read-only remount is non-recursive, so it does not clobber a writable sub-mount like `/work`, and user binds are non-recursive too, so a host sub-mount underneath the source is never pulled in silently.

The read-only guarantee holds against hostile in-sandbox code, and not only because `mount`/`umount2` are seccomp-denied. The executed process runs with an empty capability set: dropping the whole bounding set, combined with the capability transform on `execve`, zeroes the effective set (`CapEff` is `0`, verified in a running container). With no `CAP_SYS_ADMIN` it cannot remount anything, so `mount_setattr` and the rest of the modern mount API return `EPERM` at the kernel. Those syscalls are on the seccomp deny-list as well, as a second layer that would still hold if a capability were ever reintroduced.

## The MCP server

`cocoon mcp` is the agent-facing front door. It speaks JSON-RPC 2.0 over stdio with newline-delimited messages (the MCP stdio framing) and implements `initialize`, `tools/list`, `tools/call`, and notifications. It exposes one tool, `run_in_sandbox`, which turns the tool arguments into a `Config`, plans it, runs it through the same measured executor, and returns the `Outcome` JSON as the tool result text. The JSON-RPC plumbing is hand-rolled; only value parsing and serialization use `serde_json`, kept to that one module so the isolation core stays dependency-free. Requests are handled one line at a time, so the on-first-use build of the default busybox rootfs cannot race.

## Profiles

`profile = strict | build` fills in defaults so a caller does not have to spell out the same lockdown every time: `strict` for untrusted code (network off, read-only base, tight timeout and memory), `build` for producing artifacts (writable base, more memory, a longer timeout, still no network). Profiles are resolved in `Config::parse` after the whole file is read, and only for keys the file did not set explicitly, so an explicit line always wins and order does not matter. Hardening (dropped capabilities, `no_new_privs`, seccomp) is unconditional and not something a profile can turn off.

## Non-goals

No image format or layer store (a bundle is a plain rootfs directory you
provide). No network plumbing for the isolated net namespace. The seccomp filter
is a small deny-list, not a full profile. Each of those is real work and each
would bury the isolation core that is the whole point of reading this project.

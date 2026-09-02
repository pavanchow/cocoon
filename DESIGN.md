# Design

Cocoon is a container runtime you can read. Every choice serves that: a tiny surface, one concept per file, and a hard split between deciding what to do and doing it.

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

## Non-goals

No image format or layer store (a bundle is a plain rootfs directory you
provide). No network plumbing for the isolated net namespace. The seccomp filter
is a small deny-list, not a full profile. Each of those is real work and each
would bury the isolation core that is the whole point of reading this project.

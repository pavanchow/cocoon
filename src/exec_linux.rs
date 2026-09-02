//! The Linux runtime: turn a [`Plan`] into an actually-isolated process. Rootless,
//! so it needs no privileges. The sequence is the heart of every container
//! runtime, written out plainly:
//!
//! 1. `unshare` new user, mount, pid, uts, ipc (and optionally net) namespaces.
//! 2. Map our outer uid/gid to 0 inside the user namespace (this is what makes
//!    the rest work without root).
//! 3. `fork`: the child is pid 1 in the new pid namespace.
//! 4. In the child: set the hostname, make the mount tree private, `pivot_root`
//!    into the bundle's rootfs, mount a fresh `/proc`, drop the old root, then
//!    `exec` the requested process.
//! 5. The parent waits and reports the exit code.
#![cfg(target_os = "linux")]
use crate::error::{Error, Result};
use crate::plan::{Namespace, Plan};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{chdir, execvpe, fork, pivot_root, sethostname, ForkResult};
use std::ffi::CString;
use std::path::Path;

fn rt<E: std::fmt::Display>(ctx: &str) -> impl Fn(E) -> Error + '_ {
    move |e| Error::Runtime(format!("{ctx}: {e}"))
}

pub fn run(plan: &Plan, rootfs: &Path) -> Result<i32> {
    if !rootfs.is_dir() {
        return Err(Error::Runtime(format!(
            "rootfs '{}' is not a directory",
            rootfs.display()
        )));
    }
    let rootfs = rootfs.canonicalize().map_err(rt("canonicalize rootfs"))?;

    let outer_uid = nix::unistd::getuid().as_raw();
    let outer_gid = nix::unistd::getgid().as_raw();

    let mut flags = CloneFlags::empty();
    for ns in &plan.namespaces {
        flags |= match ns {
            Namespace::User => CloneFlags::CLONE_NEWUSER,
            Namespace::Mount => CloneFlags::CLONE_NEWNS,
            Namespace::Pid => CloneFlags::CLONE_NEWPID,
            Namespace::Uts => CloneFlags::CLONE_NEWUTS,
            Namespace::Ipc => CloneFlags::CLONE_NEWIPC,
            Namespace::Net => CloneFlags::CLONE_NEWNET,
        };
    }
    unshare(flags).map_err(rt(
        "unshare namespaces (is unprivileged_userns_clone enabled?)",
    ))?;

    if plan.namespaces.contains(&Namespace::User) {
        // Map our uid/gid to 0 inside the new user namespace. setgroups must be
        // denied before the gid map is allowed for a rootless mapping.
        std::fs::write("/proc/self/uid_map", format!("0 {outer_uid} 1"))
            .map_err(rt("write uid_map"))?;
        let _ = std::fs::write("/proc/self/setgroups", "deny");
        std::fs::write("/proc/self/gid_map", format!("0 {outer_gid} 1"))
            .map_err(rt("write gid_map"))?;
    }

    // The child is pid 1 in the new pid namespace.
    match unsafe { fork() }.map_err(rt("fork"))? {
        ForkResult::Parent { child } => match waitpid(child, None).map_err(rt("waitpid"))? {
            WaitStatus::Exited(_, code) => Ok(code),
            WaitStatus::Signaled(_, sig, _) => Ok(128 + sig as i32),
            other => Err(Error::Runtime(format!("unexpected wait status: {other:?}"))),
        },
        ForkResult::Child => {
            // Any failure here must not return to the caller's Rust stack.
            match child_setup(plan, &rootfs) {
                Ok(never) => match never {},
                Err(e) => {
                    eprintln!("cocoon: {e}");
                    std::process::exit(127);
                }
            }
        }
    }
}

enum Never {}

fn child_setup(plan: &Plan, rootfs: &Path) -> Result<Never> {
    if plan.namespaces.contains(&Namespace::Uts) {
        sethostname(&plan.hostname).map_err(rt("sethostname"))?;
    }

    // Make the whole mount tree private so pivot_root does not leak back to the host.
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .map_err(rt("make / private"))?;

    // pivot_root requires the new root to be a mount point: bind it onto itself.
    mount(
        Some(rootfs),
        rootfs,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .map_err(rt("bind-mount rootfs onto itself"))?;

    let oldroot = rootfs.join(".oldroot");
    std::fs::create_dir_all(&oldroot).map_err(rt("create .oldroot"))?;
    pivot_root(rootfs, &oldroot).map_err(rt("pivot_root"))?;
    chdir("/").map_err(rt("chdir /"))?;

    // A fresh /proc so pids and /proc reflect the new pid namespace.
    let _ = mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    );

    // Detach and drop the old root.
    umount2("/.oldroot", MntFlags::MNT_DETACH).map_err(rt("umount old root"))?;
    let _ = std::fs::remove_dir("/.oldroot");

    chdir(Path::new(&plan.cwd)).map_err(rt("chdir to cwd"))?;

    // Build argv and env, defaulting PATH so plain command names resolve.
    let argv: Vec<CString> = plan
        .argv
        .iter()
        .map(|a| {
            CString::new(a.as_str()).map_err(|_| Error::Runtime("argv contains a NUL byte".into()))
        })
        .collect::<Result<_>>()?;
    let mut env = plan.env.clone();
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push((
            "PATH".into(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
        ));
    }
    let envp: Vec<CString> = env
        .iter()
        .map(|(k, v)| {
            CString::new(format!("{k}={v}"))
                .map_err(|_| Error::Runtime("env contains a NUL byte".into()))
        })
        .collect::<Result<_>>()?;

    execvpe(&argv[0], &argv, &envp).map_err(rt("exec"))?;
    unreachable!("execvpe returns only on error")
}

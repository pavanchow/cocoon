//! The Linux runtime: turn a [`Plan`] into an actually-isolated, deprivileged
//! process. Rootless, so it needs no privileges. The sequence:
//!
//! 1. `unshare` new user, mount, pid, uts, ipc (and optionally net) namespaces.
//! 2. Map our outer uid/gid to 0 inside the user namespace.
//! 3. `fork`: the child is pid 1 in the new pid namespace. The parent forwards
//!    SIGTERM/SIGINT to it and waits.
//! 4. In the child: set the hostname, make the mount tree private, `pivot_root`
//!    into the bundle rootfs, mount a fresh `/proc`, bind a minimal `/dev`,
//!    optionally remount the rootfs read-only, drop capabilities and set
//!    `no_new_privs`, then `exec`.
#![cfg(target_os = "linux")]
use crate::error::{Error, Result};
use crate::plan::{Namespace, Plan};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{chdir, execvpe, fork, pivot_root, sethostname, ForkResult};
use std::ffi::CString;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

fn rt<E: std::fmt::Display>(ctx: &str) -> impl Fn(E) -> Error + '_ {
    move |e| Error::Runtime(format!("{ctx}: {e}"))
}

// Set once the child exists, so the signal handler can forward to it.
static CHILD_PID: AtomicI32 = AtomicI32::new(0);
extern "C" fn forward_signal(sig: libc::c_int) {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe { libc::kill(pid, sig) };
    }
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
        ForkResult::Parent { child } => {
            CHILD_PID.store(child.as_raw(), Ordering::SeqCst);
            let sa = SigAction::new(
                SigHandler::Handler(forward_signal),
                SaFlags::empty(),
                SigSet::empty(),
            );
            unsafe {
                let _ = sigaction(Signal::SIGTERM, &sa);
                let _ = sigaction(Signal::SIGINT, &sa);
            }
            // waitpid can be interrupted by a forwarded signal; retry.
            loop {
                match waitpid(child, None) {
                    Ok(WaitStatus::Exited(_, code)) => return Ok(code),
                    Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + sig as i32),
                    Ok(other) => {
                        return Err(Error::Runtime(format!("unexpected wait status: {other:?}")))
                    }
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(e) => return Err(rt("waitpid")(e)),
                }
            }
        }
        ForkResult::Child => match child_setup(plan, &rootfs) {
            Ok(never) => match never {},
            Err(e) => {
                eprintln!("cocoon: {e}");
                std::process::exit(127);
            }
        },
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

    // A fresh /proc so pids and /proc reflect the new pid namespace. If a pid
    // namespace was requested this must succeed, or the view would be wrong.
    if plan.namespaces.contains(&Namespace::Pid) {
        mount(
            Some("proc"),
            "/proc",
            Some("proc"),
            MsFlags::empty(),
            None::<&str>,
        )
        .map_err(rt("mount /proc"))?;
    }

    setup_dev();

    // The old root is still visible under /.oldroot; drop it now.
    umount2("/.oldroot", MntFlags::MNT_DETACH).map_err(rt("umount old root"))?;
    let _ = std::fs::remove_dir("/.oldroot");

    if plan.readonly {
        mount(
            None::<&str>,
            "/",
            None::<&str>,
            MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .map_err(rt("remount rootfs read-only"))?;
    }

    chdir(Path::new(&plan.cwd)).map_err(rt("chdir to cwd"))?;
    deprivilege();

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

/// Bind the essential device nodes from the old root into a minimal `/dev`.
/// Rootless cannot `mknod`, so we bind the host's existing nodes (still visible
/// under /.oldroot at this point). Best effort per node.
fn setup_dev() {
    let _ = std::fs::create_dir_all("/dev");
    for node in ["null", "zero", "full", "random", "urandom", "tty"] {
        let src = format!("/.oldroot/dev/{node}");
        let dst = format!("/dev/{node}");
        if Path::new(&src).exists() {
            let _ = std::fs::File::create(&dst);
            let _ = mount(
                Some(src.as_str()),
                dst.as_str(),
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            );
        }
    }
}

/// Set `no_new_privs`, clear the ambient capability set, and drop the whole
/// capability bounding set, so the container process and anything it execs
/// cannot gain privileges.
fn deprivilege() {
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        );
        // CAP_LAST_CAP is around 40; dropping past it is a harmless EINVAL.
        for cap in 0..=63 {
            libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0);
        }
    }
}

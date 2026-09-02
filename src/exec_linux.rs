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
use crate::config::Mount;
use crate::error::{Error, Result};
use crate::plan::{Namespace, Plan};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::resource::{getrusage, UsageWho};
use nix::sys::signal::{kill, sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{chdir, execvpe, fork, pivot_root, sethostname, ForkResult};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

fn rt<E: std::fmt::Display>(ctx: &str) -> impl Fn(E) -> Error + '_ {
    move |e| Error::Runtime(format!("{ctx}: {e}"))
}

// Read all currently-available bytes from a nonblocking fd into `buf`. On EOF
// (read returns 0) it closes the fd and clears `open`; on EAGAIN it stops until
// the next poll. Used to drain the captured stdout/stderr pipes without threads.
fn drain_fd(fd: i32, open: &mut bool, buf: &mut Vec<u8>) {
    if !*open {
        return;
    }
    let mut tmp = [0u8; 8192];
    loop {
        let n = unsafe { libc::read(fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
        if n > 0 {
            buf.extend_from_slice(&tmp[..n as usize]);
        } else if n == 0 {
            *open = false;
            unsafe { libc::close(fd) };
            return;
        } else {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EAGAIN) => return,
                Some(libc::EINTR) => continue,
                _ => {
                    *open = false;
                    unsafe { libc::close(fd) };
                    return;
                }
            }
        }
    }
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

    // Best-effort cgroup v2 limits (needs a delegated cgroup; warns and
    // continues if none is available).
    let cgroup = setup_cgroup(&plan.limits);

    // The child is pid 1 in the new pid namespace.
    match unsafe { fork() }.map_err(rt("fork"))? {
        ForkResult::Parent { child } => {
            if let Some(cg) = &cgroup {
                let _ = std::fs::write(cg.join("cgroup.procs"), child.as_raw().to_string());
            }
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
            let result = loop {
                match waitpid(child, None) {
                    Ok(WaitStatus::Exited(_, code)) => break Ok(code),
                    Ok(WaitStatus::Signaled(_, sig, _)) => break Ok(128 + sig as i32),
                    Ok(other) => {
                        break Err(Error::Runtime(format!("unexpected wait status: {other:?}")))
                    }
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(e) => break Err(rt("waitpid")(e)),
                }
            };
            if let Some(cg) = &cgroup {
                let _ = std::fs::remove_dir(cg);
            }
            result
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

/// Run the container while capturing stdout/stderr and measuring it, enforcing an
/// optional wall-clock timeout. Returns a structured [`Outcome`].
pub fn run_measured(plan: &Plan, rootfs: &Path) -> Result<crate::Outcome> {
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
    unshare(flags).map_err(rt("unshare namespaces"))?;
    if plan.namespaces.contains(&Namespace::User) {
        std::fs::write("/proc/self/uid_map", format!("0 {outer_uid} 1")).map_err(rt("write uid_map"))?;
        let _ = std::fs::write("/proc/self/setgroups", "deny");
        std::fs::write("/proc/self/gid_map", format!("0 {outer_gid} 1")).map_err(rt("write gid_map"))?;
    }

    // Pipes to capture the child's stdout and stderr.
    let mut op = [0i32; 2];
    let mut ep = [0i32; 2];
    if unsafe { libc::pipe(op.as_mut_ptr()) } != 0 || unsafe { libc::pipe(ep.as_mut_ptr()) } != 0 {
        return Err(Error::Runtime("pipe failed".into()));
    }
    let cgroup = setup_cgroup(&plan.limits);
    let start = Instant::now();

    match unsafe { fork() }.map_err(rt("fork"))? {
        ForkResult::Child => {
            unsafe {
                libc::dup2(op[1], 1);
                libc::dup2(ep[1], 2);
                libc::close(op[0]);
                libc::close(op[1]);
                libc::close(ep[0]);
                libc::close(ep[1]);
            }
            match child_setup(plan, &rootfs) {
                Ok(never) => match never {},
                Err(e) => {
                    eprintln!("cocoon: {e}");
                    std::process::exit(127);
                }
            }
        }
        ForkResult::Parent { child } => {
            if let Some(cg) = &cgroup {
                let _ = std::fs::write(cg.join("cgroup.procs"), child.as_raw().to_string());
            }
            unsafe {
                libc::close(op[1]);
                libc::close(ep[1]);
                // Nonblocking so one silent stream never stalls the drain loop.
                libc::fcntl(op[0], libc::F_SETFL, libc::O_NONBLOCK);
                libc::fcntl(ep[0], libc::F_SETFL, libc::O_NONBLOCK);
            }

            // Drain both pipes with a poll loop in this same thread. We cannot use
            // reader threads: after unshare(CLONE_NEWPID) the kernel refuses to
            // create threads in this process (a new thread cannot cross the pid
            // namespace boundary), so std::thread::spawn fails EINVAL here. Polling
            // in-line drains as data arrives, so a chatty child never deadlocks on a
            // full pipe, while we also poll for exit and the timeout.
            let deadline = plan.timeout_ms.map(Duration::from_millis);
            let mut timed_out = false;
            let mut out_buf = Vec::new();
            let mut err_buf = Vec::new();
            let mut out_open = true;
            let mut err_open = true;
            let mut reaped: Option<WaitStatus> = None;
            let mut reap_at: Option<Instant> = None;
            let mut wall_ms = 0u128;
            // Once pid 1 is reaped the kernel tears down its pid namespace, so any
            // straggler still holding a pipe write end dies and the fd EOFs. Untrusted
            // code should not be able to wedge us regardless, so cap the post-reap
            // drain and force the fds closed if a straggler lingers past it.
            const DRAIN_GRACE: Duration = Duration::from_millis(500);
            loop {
                // Only the still-open fds go to poll; a closed fd would spin on POLLNVAL.
                let mut pfds: [libc::pollfd; 2] =
                    [libc::pollfd { fd: -1, events: libc::POLLIN, revents: 0 }; 2];
                let mut n = 0usize;
                if out_open {
                    pfds[n].fd = op[0];
                    n += 1;
                }
                if err_open {
                    pfds[n].fd = ep[0];
                    n += 1;
                }
                if n > 0 {
                    // Cap the wait so we re-check exit and the deadline promptly.
                    let mut wait_ms = 20i64;
                    if let Some(d) = deadline {
                        let el = start.elapsed();
                        wait_ms = if el >= d { 0 } else { wait_ms.min((d - el).as_millis() as i64) };
                    }
                    unsafe { libc::poll(pfds.as_mut_ptr(), n as libc::nfds_t, wait_ms as libc::c_int) };
                } else if reaped.is_none() {
                    std::thread::sleep(Duration::from_millis(3));
                }
                drain_fd(op[0], &mut out_open, &mut out_buf);
                drain_fd(ep[0], &mut err_open, &mut err_buf);

                if reaped.is_none() {
                    match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                        Ok(WaitStatus::StillAlive) => {
                            if let Some(d) = deadline {
                                if start.elapsed() >= d {
                                    let _ = kill(child, Signal::SIGKILL);
                                    timed_out = true;
                                }
                            }
                        }
                        Ok(s) => {
                            wall_ms = start.elapsed().as_millis();
                            reaped = Some(s);
                            reap_at = Some(Instant::now());
                        }
                        Err(nix::errno::Errno::EINTR) => {}
                        Err(e) => {
                            let _ = kill(child, Signal::SIGKILL);
                            if out_open {
                                unsafe { libc::close(op[0]) };
                            }
                            if err_open {
                                unsafe { libc::close(ep[0]) };
                            }
                            if let Some(cg) = &cgroup {
                                let _ = std::fs::remove_dir(cg);
                            }
                            return Err(rt("waitpid")(e));
                        }
                    }
                }
                if reaped.is_some() {
                    let grace_expired = reap_at.map_or(false, |t| t.elapsed() >= DRAIN_GRACE);
                    if (!out_open && !err_open) || grace_expired {
                        if out_open {
                            unsafe { libc::close(op[0]) };
                        }
                        if err_open {
                            unsafe { libc::close(ep[0]) };
                        }
                        break;
                    }
                }
            }
            let status = reaped.expect("loop only breaks after the child is reaped");
            let exit_code = match status {
                WaitStatus::Exited(_, c) => c,
                WaitStatus::Signaled(_, s, _) => 128 + s as i32,
                _ => -1,
            };
            let peak_mem_kib = getrusage(UsageWho::RUSAGE_CHILDREN)
                .ok()
                .map(|u| u.max_rss() as u64);
            let mut oom_killed = false;
            if let Some(cg) = &cgroup {
                if let Ok(ev) = std::fs::read_to_string(cg.join("memory.events")) {
                    for l in ev.lines() {
                        if let Some(n) = l.strip_prefix("oom_kill ") {
                            if n.trim().parse::<u64>().unwrap_or(0) > 0 {
                                oom_killed = true;
                            }
                        }
                    }
                }
            }
            if !oom_killed && !timed_out && plan.limits.memory_max.is_some() {
                if let WaitStatus::Signaled(_, Signal::SIGKILL, _) = status {
                    oom_killed = true;
                }
            }
            if let Some(cg) = &cgroup {
                let _ = std::fs::remove_dir(cg);
            }
            let stdout = String::from_utf8_lossy(&out_buf).into_owned();
            let stderr = String::from_utf8_lossy(&err_buf).into_owned();
            Ok(crate::Outcome {
                exit_code,
                timed_out,
                oom_killed,
                wall_ms,
                peak_mem_kib,
                stdout,
                stderr,
            })
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

    // Bind the requested host directories in. The host tree is still reachable
    // under /.oldroot, so this must happen before that old root is detached.
    for m in &plan.mounts {
        apply_bind_mount(m)?;
    }

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
    apply_seccomp();

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

/// Bind one host directory (still reachable under `/.oldroot`) onto its target
/// inside the new rootfs. A read-only mount is bound first, then remounted
/// read-only, since a bind mount cannot be made read-only in a single step.
fn apply_bind_mount(m: &Mount) -> Result<()> {
    let src = PathBuf::from("/.oldroot").join(m.source.trim_start_matches('/'));
    if !src.exists() {
        return Err(Error::Runtime(format!(
            "mount source '{}' does not exist on the host",
            m.source
        )));
    }
    let dst = PathBuf::from(&m.target);
    if src.is_dir() {
        std::fs::create_dir_all(&dst).map_err(rt("create mount target"))?;
    } else {
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&dst);
    }
    mount(
        Some(&src),
        &dst,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )
    .map_err(rt("bind mount"))?;
    if m.readonly {
        mount(
            None::<&str>,
            &dst,
            None::<&str>,
            MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .map_err(rt("remount mount read-only"))?;
    }
    Ok(())
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

/// Create a child cgroup v2 under our delegated cgroup and write the requested
/// memory/pids limits. Rootless enforcement needs the cgroup subtree to be
/// delegated to the user; if it is not, this warns and returns None and the
/// container runs without enforced limits (isolation is unaffected).
fn setup_cgroup(limits: &crate::plan::Limits) -> Option<PathBuf> {
    if !limits.any() {
        return None;
    }
    let give_up = || {
        eprintln!(
            "cocoon: cgroup limits requested but no delegated writable cgroup v2 is available; \
             continuing without enforcement"
        );
        None
    };
    let mine = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = mine.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    let base = PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/'));

    // cgroup v2 forbids a cgroup from holding processes and enabling controllers
    // for its children at the same time. Move ourselves into a leaf manager
    // cgroup so `base` has no direct processes, then we can enable controllers.
    let mgr = base.join("cocoon-mgr");
    if std::fs::create_dir_all(&mgr).is_err() {
        return give_up();
    }
    if std::fs::write(mgr.join("cgroup.procs"), std::process::id().to_string()).is_err() {
        return give_up();
    }
    if std::fs::write(base.join("cgroup.subtree_control"), "+memory +pids").is_err() {
        return give_up();
    }
    let cg = base.join(format!("cocoon-{}", std::process::id()));
    if std::fs::create_dir_all(&cg).is_err() {
        return give_up();
    }
    // If a limit cannot actually be written, do not pretend it is enforced.
    if let Some(m) = limits.memory_max {
        if std::fs::write(cg.join("memory.max"), m.to_string()).is_err() {
            eprintln!("cocoon: could not set memory.max; continuing without that limit");
        }
    }
    if let Some(p) = limits.pids_max {
        if std::fs::write(cg.join("pids.max"), p.to_string()).is_err() {
            eprintln!("cocoon: could not set pids.max; continuing without that limit");
        }
    }
    Some(cg)
}

/// Install a small seccomp filter: allow syscalls by default, deny a curated set
/// of dangerous ones with EPERM. Applied last, after no_new_privs, so an
/// unprivileged process is permitted to load it.
fn apply_seccomp() {
    use seccompiler::{SeccompAction, SeccompFilter, TargetArch};
    use std::collections::BTreeMap;

    let deny: &[libc::c_long] = &[
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_ptrace,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_reboot,
        libc::SYS_kexec_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
    ];
    let mut rules = BTreeMap::new();
    for &s in deny {
        rules.insert(s as i64, vec![]);
    }
    let arch = if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        TargetArch::x86_64
    };
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    );
    if let Ok(f) = filter {
        if let Ok(prog) = TryInto::<seccompiler::BpfProgram>::try_into(f) {
            let _ = seccompiler::apply_filter(&prog);
        }
    }
}

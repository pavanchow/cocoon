//! End-to-end test of the Linux executor. Builds a minimal busybox rootfs and
//! runs the real `cocoon` binary as a subprocess, asserting a clean exit. Skips
//! if busybox is not installed. Runs on Linux only.
//!
//! It spawns the binary rather than calling `run_plan` in-process on purpose:
//! `unshare(CLONE_NEWUSER)` requires a single-threaded caller, and the cargo
//! test harness is multi-threaded, so an in-process unshare would fail EINVAL.
#![cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;

fn build_rootfs(dir: &Path) -> bool {
    let busybox = ["/usr/bin/busybox", "/bin/busybox"]
        .iter()
        .map(Path::new)
        .find(|p| p.exists());
    let bb = match busybox {
        Some(p) => p,
        None => return false,
    };
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(dir.join("proc")).unwrap();
    std::fs::copy(bb, bin.join("busybox")).unwrap();
    for a in ["sh", "echo", "id", "hostname"] {
        let _ = std::os::unix::fs::symlink("busybox", bin.join(a));
    }
    if let Ok(out) = Command::new("ldd").arg(bb).output() {
        for tok in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            if tok.starts_with('/') && tok.contains(".so") {
                let src = Path::new(tok);
                if src.exists() {
                    let dst = dir.join(src.strip_prefix("/").unwrap());
                    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
                    let _ = std::fs::copy(src, dst);
                }
            }
        }
    }
    true
}

#[test]
fn runs_a_container_to_a_clean_exit() {
    let bundle = std::env::temp_dir().join(format!("cocoon-test-{}", std::process::id()));
    let rootfs = bundle.join("rootfs");
    std::fs::create_dir_all(&rootfs).unwrap();
    if !build_rootfs(&rootfs) {
        eprintln!("busybox not found, skipping executor test");
        return;
    }
    std::fs::write(
        bundle.join("cocoon.conf"),
        "hostname = testbox\nargv = /bin/busybox sh -c \"id -u\"\nenv = PATH=/bin\n",
    )
    .unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_cocoon"))
        .arg("run")
        .arg(&bundle)
        .status()
        .expect("spawn cocoon");
    let _ = std::fs::remove_dir_all(&bundle);
    assert!(status.success(), "cocoon run should exit 0, got {status:?}");
}

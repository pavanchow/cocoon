//! End-to-end test of `cocoon exec --json`: build a busybox rootfs, run the real
//! binary, and assert the emitted Outcome JSON has the measured fields and the
//! captured stdout. Skips (like `linux_exec`) when rootless user namespaces are
//! unavailable, e.g. on GitHub Actions runners. Linux only.
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
    for a in ["sh", "echo"] {
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
fn exec_json_reports_measured_fields() {
    let bundle = std::env::temp_dir().join(format!("cocoon-exec-{}", std::process::id()));
    let rootfs = bundle.join("rootfs");
    std::fs::create_dir_all(&rootfs).unwrap();
    if !build_rootfs(&rootfs) {
        eprintln!("busybox not found, skipping measured exec test");
        return;
    }
    std::fs::write(
        bundle.join("cocoon.conf"),
        "hostname = testbox\nargv = /bin/busybox echo hello-cocoon\nenv = PATH=/bin\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_cocoon"))
        .args(["exec", "--json"])
        .arg(&bundle)
        .output()
        .expect("spawn cocoon");
    let _ = std::fs::remove_dir_all(&bundle);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // No JSON on stdout means the run did not happen. That is a real failure
    // unless this environment forbids rootless user namespaces, in which case skip.
    if !stdout.contains("\"exit_code\"") {
        if stderr.contains("Operation not permitted")
            || stderr.contains("unshare")
            || stderr.contains("uid_map")
        {
            eprintln!(
                "rootless user namespaces unavailable here, skipping: {}",
                stderr.trim()
            );
            return;
        }
        panic!("cocoon exec produced no Outcome JSON: {}", stderr.trim());
    }

    for field in [
        "\"exit_code\":0",
        "\"timed_out\":false",
        "\"oom_killed\":false",
        "\"wall_ms\":",
        "\"peak_mem_kib\":",
        "\"stdout\":\"hello-cocoon\\n\"",
    ] {
        assert!(stdout.contains(field), "missing {field} in: {stdout}");
    }
}

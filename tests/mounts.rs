//! End-to-end test of the `mount` policy: a rw bind must let the container write
//! back to a host directory, and a ro bind must reject writes. Runs the real
//! binary against a busybox rootfs. Skips (like `linux_exec`) when rootless user
//! namespaces are unavailable. Linux only.
#![cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;

fn build_rootfs(dir: &Path) -> bool {
    let bb = match ["/usr/bin/busybox", "/bin/busybox"]
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
    {
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
fn rw_mount_persists_and_ro_mount_blocks() {
    let base = std::env::temp_dir().join(format!("cocoon-mnt-{}", std::process::id()));
    let rootfs = base.join("rootfs");
    let host_rw = base.join("rw");
    let host_ro = base.join("ro");
    std::fs::create_dir_all(&rootfs).unwrap();
    std::fs::create_dir_all(&host_rw).unwrap();
    std::fs::create_dir_all(&host_ro).unwrap();
    if !build_rootfs(&rootfs) {
        eprintln!("busybox not found, skipping mount test");
        return;
    }
    std::fs::write(
        base.join("cocoon.conf"),
        format!(
            "hostname = mnttest\ncwd = /\n\
             mount = {}:/work:rw\nmount = {}:/in:ro\n\
             argv = /bin/busybox sh -c \"echo persisted > /work/result.txt; echo nope > /in/blocked.txt\"\n\
             env = PATH=/bin\n",
            host_rw.display(),
            host_ro.display()
        ),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_cocoon"))
        .arg("exec")
        .arg(&base)
        .output()
        .expect("spawn cocoon");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let result = host_rw.join("result.txt");
    let blocked = host_ro.join("blocked.txt");
    let rw_ok = result.exists();
    let ro_leaked = blocked.exists();
    let rw_contents = std::fs::read_to_string(&result).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&base);

    if !rw_ok {
        if stderr.contains("Operation not permitted")
            || stderr.contains("unshare")
            || stderr.contains("uid_map")
        {
            eprintln!("rootless user namespaces unavailable here, skipping: {}", stderr.trim());
            return;
        }
        panic!("rw mount did not persist to the host: {}", stderr.trim());
    }
    assert_eq!(rw_contents, "persisted\n", "rw mount content mismatch");
    assert!(!ro_leaked, "ro mount was writable: the write reached the host");
}

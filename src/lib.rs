//! Cocoon: a small rootless Linux container runtime you can read end to end.
//!
//! A **bundle** is a directory with a `rootfs/` subdirectory and a `cocoon.conf`
//! file. [`Config`] parses the file, [`Plan`] turns it into the decision to
//! execute (no side effects, testable anywhere), and on Linux the executor
//! unshares namespaces, `pivot_root`s into the rootfs, and runs the process.
//! The lifecycle is modeled by [`Container`].
pub mod config;
pub mod error;
pub mod plan;
pub mod state;

#[cfg(target_os = "linux")]
mod exec_linux;

pub use config::Config;
pub use error::{Error, Result};
pub use plan::{Namespace, Plan};
pub use state::{Container, State};

use std::path::{Path, PathBuf};

/// The structured result of a measured run: what a caller (a CI script, or an
/// AI agent) gets back from one sandboxed execution.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub exit_code: i32,
    pub timed_out: bool,
    pub oom_killed: bool,
    pub wall_ms: u128,
    pub peak_mem_kib: Option<u64>,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    /// Serialize to JSON by hand, so the crate stays dependency-free.
    pub fn to_json(&self) -> String {
        fn s(v: &str) -> String {
            let mut o = String::from("\"");
            for c in v.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\r' => o.push_str("\\r"),
                    '\t' => o.push_str("\\t"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        }
        let peak = match self.peak_mem_kib {
            Some(k) => k.to_string(),
            None => "null".into(),
        };
        format!(
            "{{\"exit_code\":{},\"timed_out\":{},\"oom_killed\":{},\"wall_ms\":{},\"peak_mem_kib\":{},\"stdout\":{},\"stderr\":{}}}",
            self.exit_code, self.timed_out, self.oom_killed, self.wall_ms, peak,
            s(&self.stdout), s(&self.stderr)
        )
    }
}

pub struct Bundle {
    pub dir: PathBuf,
    pub rootfs: PathBuf,
    pub config: Config,
}

/// Read and parse `<dir>/cocoon.conf`. Does not require the rootfs to exist, so
/// a bundle can be inspected (`cocoon plan`) before it is fully assembled.
pub fn load_bundle(dir: &Path) -> Result<Bundle> {
    let conf = dir.join("cocoon.conf");
    let text = std::fs::read_to_string(&conf)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", conf.display())))?;
    let config = Config::parse(&text)?;
    Ok(Bundle {
        dir: dir.to_path_buf(),
        rootfs: dir.join("rootfs"),
        config,
    })
}

/// Compute the plan for a bundle without running it.
pub fn plan_bundle(dir: &Path) -> Result<Plan> {
    Plan::from_config(&load_bundle(dir)?.config)
}

/// Run a bundle to completion, returning the process exit code.
pub fn run(dir: &Path) -> Result<i32> {
    let bundle = load_bundle(dir)?;
    let plan = Plan::from_config(&bundle.config)?;
    run_plan(&plan, &bundle.rootfs)
}

#[cfg(target_os = "linux")]
pub fn run_plan(plan: &Plan, rootfs: &Path) -> Result<i32> {
    exec_linux::run(plan, rootfs)
}

#[cfg(not(target_os = "linux"))]
pub fn run_plan(_plan: &Plan, _rootfs: &Path) -> Result<i32> {
    Err(Error::Unsupported(
        "running containers needs Linux namespaces; on this OS you can still parse the config \
         and inspect the plan with `cocoon plan <bundle>`"
            .into(),
    ))
}

/// Run a bundle in the sandbox and return a structured [`Outcome`]: captured
/// stdout and stderr, exit code, wall time, peak memory, and whether it timed
/// out or was OOM-killed. This is the API an agent or a script uses.
pub fn run_measured(dir: &Path) -> Result<Outcome> {
    let bundle = load_bundle(dir)?;
    let plan = Plan::from_config(&bundle.config)?;
    run_measured_plan(&plan, &bundle.rootfs)
}

#[cfg(target_os = "linux")]
pub fn run_measured_plan(plan: &Plan, rootfs: &Path) -> Result<Outcome> {
    exec_linux::run_measured(plan, rootfs)
}

#[cfg(not(target_os = "linux"))]
pub fn run_measured_plan(_plan: &Plan, _rootfs: &Path) -> Result<Outcome> {
    Err(Error::Unsupported(
        "sandboxed execution needs Linux namespaces".into(),
    ))
}

/// The default `cocoon.conf` written by `cocoon spec`.
pub const DEFAULT_CONFIG: &str = "# cocoon bundle config\n\
hostname = cocoon\n\
cwd      = /\n\
argv     = /bin/sh\n\
env      = PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
# net        = isolated\n\
# readonly   = true\n\
# timeout    = 5s               # 5s, 500ms, 2m, or a bare number of seconds\n\
# mount      = /host/dir:/work:rw   # bind a host dir in (ro or rw); may repeat\n\
# memory_max = 67108864\n\
# pids_max   = 64\n";

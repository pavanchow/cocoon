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

/// The default `cocoon.conf` written by `cocoon spec`.
pub const DEFAULT_CONFIG: &str = "# cocoon bundle config\n\
hostname = cocoon\n\
cwd      = /\n\
argv     = /bin/sh\n\
env      = PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
# net        = isolated\n\
# memory_max = 67108864\n\
# pids_max   = 64\n";

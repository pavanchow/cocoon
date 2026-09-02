//! The Plan is the decision the runtime executes: which namespaces to unshare,
//! what to run, and which limits to apply. It is computed from a [`Config`] with
//! no side effects, so the whole decision can be unit-tested on any OS. The
//! Linux executor consumes a Plan; it never re-reads the config.
use crate::config::{Config, Mount};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    User,
    Mount,
    Pid,
    Uts,
    Ipc,
    Net,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub memory_max: Option<u64>,
    pub pids_max: Option<u64>,
}

impl Limits {
    pub fn any(&self) -> bool {
        self.memory_max.is_some() || self.pids_max.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub namespaces: Vec<Namespace>,
    pub hostname: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub readonly: bool,
    pub timeout_ms: Option<u64>,
    pub limits: Limits,
    pub mounts: Vec<Mount>,
}

impl Plan {
    /// Build the plan from a validated config. Rootless containers always use a
    /// user namespace (so no root is required); mount, pid, uts, and ipc give
    /// the isolation. The network namespace is opt-in because, without extra
    /// setup, isolating it leaves the container with no connectivity.
    pub fn from_config(cfg: &Config) -> Result<Plan> {
        if cfg.argv.is_empty() {
            return Err(Error::Plan("argv is empty; nothing to run".into()));
        }
        if !cfg.cwd.starts_with('/') {
            return Err(Error::Plan(format!(
                "cwd must be an absolute path, got '{}'",
                cfg.cwd
            )));
        }
        let mut namespaces = vec![
            Namespace::User,
            Namespace::Mount,
            Namespace::Pid,
            Namespace::Uts,
            Namespace::Ipc,
        ];
        if cfg.isolate_net {
            namespaces.push(Namespace::Net);
        }
        Ok(Plan {
            namespaces,
            hostname: cfg.hostname.clone(),
            cwd: cfg.cwd.clone(),
            argv: cfg.argv.clone(),
            env: cfg.env.clone(),
            readonly: cfg.readonly,
            timeout_ms: cfg.timeout_ms,
            limits: Limits {
                memory_max: cfg.memory_max,
                pids_max: cfg.pids_max,
            },
            mounts: cfg.mounts.clone(),
        })
    }

    /// A human-readable summary of what running this plan will do. Used by
    /// `cocoon plan <bundle>` so the decision can be inspected before executing.
    pub fn describe(&self) -> String {
        let ns: Vec<&str> = self
            .namespaces
            .iter()
            .map(|n| match n {
                Namespace::User => "user",
                Namespace::Mount => "mount",
                Namespace::Pid => "pid",
                Namespace::Uts => "uts",
                Namespace::Ipc => "ipc",
                Namespace::Net => "net",
            })
            .collect();
        let mut s = String::new();
        s.push_str(&format!("namespaces : {}\n", ns.join(", ")));
        s.push_str(&format!("hostname   : {}\n", self.hostname));
        s.push_str(&format!("cwd        : {}\n", self.cwd));
        s.push_str(&format!("argv       : {}\n", self.argv.join(" ")));
        s.push_str(&format!(
            "rootfs     : {}\n",
            if self.readonly {
                "read-only"
            } else {
                "read-write"
            }
        ));
        s.push_str("hardening  : no_new_privs, dropped capabilities, minimal /dev\n");
        if !self.env.is_empty() {
            s.push_str(&format!("env        : {} var(s)\n", self.env.len()));
        }
        if let Some(m) = self.limits.memory_max {
            s.push_str(&format!("memory_max : {m} bytes\n"));
        }
        if let Some(p) = self.limits.pids_max {
            s.push_str(&format!("pids_max   : {p}\n"));
        }
        if let Some(t) = self.timeout_ms {
            s.push_str(&format!("timeout    : {t} ms\n"));
        }
        for m in &self.mounts {
            s.push_str(&format!(
                "mount      : {} -> {} ({})\n",
                m.source,
                m.target,
                if m.readonly { "ro" } else { "rw" }
            ));
        }
        s
    }
}

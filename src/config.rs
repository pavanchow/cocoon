//! The bundle config: a small, hand-parsed, line-based format. A bundle is a
//! directory with a `rootfs/` subdirectory and a `cocoon.conf` file.
//!
//! Format (one `key = value` per line, `#` comments, blank lines ignored):
//!   hostname   = cocoon
//!   cwd        = /
//!   argv       = /bin/sh -c "echo hello from $(hostname)"
//!   env        = PATH=/usr/local/bin:/usr/bin:/bin        # may repeat
//!   net        = host | isolated                          # default host
//!   memory_max = 67108864                                 # bytes, optional
//!   pids_max   = 64                                       # optional
use crate::error::{Error, Result};

/// A host directory bound into the container. Lets an agent hand the sandbox a
/// writable `/work` and read results back, or share inputs read-only.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub readonly: bool,
}

/// A named bundle of sensible defaults. `strict` locks a run down for untrusted
/// code (network off, read-only base, tight timeout and memory); `build` loosens
/// it for compiling and generating artifacts (writable base, more memory, a
/// longer timeout), still with the network off. Hardening (dropped capabilities,
/// `no_new_privs`, seccomp) is always on and not affected by the profile.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Profile {
    Strict,
    Build,
}

impl Profile {
    fn apply(self, c: &mut Config, seen: &std::collections::HashSet<&str>) {
        let (isolate_net, readonly, timeout_ms, memory_max) = match self {
            Profile::Strict => (true, true, 5_000, 128 * 1024 * 1024),
            Profile::Build => (true, false, 300_000, 1024 * 1024 * 1024),
        };
        if !seen.contains("net") {
            c.isolate_net = isolate_net;
        }
        if !seen.contains("readonly") {
            c.readonly = readonly;
        }
        if !seen.contains("timeout") {
            c.timeout_ms = Some(timeout_ms);
        }
        if !seen.contains("memory_max") {
            c.memory_max = Some(memory_max);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub hostname: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub isolate_net: bool,
    pub readonly: bool,
    pub memory_max: Option<u64>,
    pub pids_max: Option<u64>,
    /// Wall-clock limit in milliseconds; the container is killed if it exceeds it.
    pub timeout_ms: Option<u64>,
    /// Host directories bound into the rootfs. May repeat.
    pub mounts: Vec<Mount>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: "cocoon".into(),
            cwd: "/".into(),
            argv: Vec::new(),
            env: Vec::new(),
            isolate_net: false,
            readonly: false,
            memory_max: None,
            pids_max: None,
            timeout_ms: None,
            mounts: Vec::new(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let mut c = Config::default();
        let mut saw_argv = false;
        let mut profile: Option<Profile> = None;
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (i, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| Error::Config(format!("line {}: expected 'key = value'", i + 1)))?;
            let key = key.trim();
            let value = value.trim();
            seen.insert(key);
            match key {
                "hostname" => c.hostname = value.to_string(),
                "cwd" => c.cwd = value.to_string(),
                "argv" => {
                    c.argv = split_args(value)
                        .map_err(|e| Error::Config(format!("line {}: {e}", i + 1)))?;
                    saw_argv = true;
                }
                "env" => {
                    let (k, v) = value.split_once('=').ok_or_else(|| {
                        Error::Config(format!("line {}: env must be KEY=VALUE", i + 1))
                    })?;
                    c.env.push((k.trim().to_string(), v.to_string()));
                }
                "net" => {
                    c.isolate_net = match value {
                        "host" => false,
                        "isolated" => true,
                        other => {
                            return Err(Error::Config(format!(
                                "line {}: net must be 'host' or 'isolated', got '{other}'",
                                i + 1
                            )))
                        }
                    }
                }
                "readonly" => {
                    c.readonly = match value {
                        "true" => true,
                        "false" => false,
                        other => {
                            return Err(Error::Config(format!(
                                "line {}: readonly must be 'true' or 'false', got '{other}'",
                                i + 1
                            )))
                        }
                    }
                }
                "memory_max" => c.memory_max = Some(parse_u64(value, i + 1, "memory_max")?),
                "pids_max" => c.pids_max = Some(parse_u64(value, i + 1, "pids_max")?),
                "timeout" => c.timeout_ms = Some(parse_duration(value, i + 1)?),
                "mount" => c.mounts.push(parse_mount(value, i + 1)?),
                "profile" => {
                    profile = Some(match value {
                        "strict" => Profile::Strict,
                        "build" => Profile::Build,
                        other => {
                            return Err(Error::Config(format!(
                                "line {}: profile must be 'strict' or 'build', got '{other}'",
                                i + 1
                            )))
                        }
                    })
                }
                other => {
                    return Err(Error::Config(format!(
                        "line {}: unknown key '{other}'",
                        i + 1
                    )))
                }
            }
        }
        // A profile fills in defaults, but only for keys not set explicitly, so an
        // explicit line always wins regardless of where it sits in the file.
        if let Some(p) = profile {
            p.apply(&mut c, &seen);
        }
        if !saw_argv || c.argv.is_empty() {
            return Err(Error::Config("config must set a non-empty 'argv'".into()));
        }
        Ok(c)
    }
}

// A `#` starts a comment only when it is outside double quotes and at the start
// of the line or preceded by whitespace. So `#` inside a quoted argv is kept, an
// unspaced `a#b` in a value is kept, and `key = val  # note` still strips.
fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            '#' if !in_quote && (i == 0 || prev.is_whitespace()) => return &line[..i],
            _ => {}
        }
        prev = c;
    }
    line
}

fn parse_u64(v: &str, line: usize, key: &str) -> Result<u64> {
    v.parse::<u64>()
        .map_err(|_| Error::Config(format!("line {line}: {key} must be a non-negative integer")))
}

/// Parse a duration like `5s`, `500ms`, `2m`, or a bare number of seconds, to
/// milliseconds. Returns None if it does not parse. Shared by the config, the
/// `--timeout` flag, and the MCP tool so all three accept the same forms.
pub fn parse_timeout_ms(v: &str) -> Option<u64> {
    let v = v.trim();
    let (num, mult): (&str, u64) = if let Some(n) = v.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = v.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = v.strip_suffix('m') {
        (n, 60_000)
    } else {
        (v, 1000)
    };
    num.trim().parse::<u64>().ok().and_then(|n| n.checked_mul(mult))
}

fn parse_duration(v: &str, line: usize) -> Result<u64> {
    parse_timeout_ms(v).ok_or_else(|| {
        Error::Config(format!(
            "line {line}: timeout must be like '5s', '500ms', '2m', or a number of seconds"
        ))
    })
}

/// Parse `SOURCE:TARGET:ro|rw`, e.g. `mount = /host/data:/work:rw`. Both paths
/// must be absolute and the flag must be `ro` or `rw`.
fn parse_mount(v: &str, line: usize) -> Result<Mount> {
    let parts: Vec<&str> = v.split(':').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(Error::Config(format!(
            "line {line}: mount must be 'SOURCE:TARGET:ro|rw'"
        )));
    }
    let (source, target, flag) = (parts[0], parts[1], parts[2]);
    if !source.starts_with('/') || !target.starts_with('/') {
        return Err(Error::Config(format!(
            "line {line}: mount source and target must be absolute paths"
        )));
    }
    let readonly = match flag {
        "ro" => true,
        "rw" => false,
        other => {
            return Err(Error::Config(format!(
                "line {line}: mount flag must be 'ro' or 'rw', got '{other}'"
            )))
        }
    };
    Ok(Mount {
        source: source.to_string(),
        target: target.to_string(),
        readonly,
    })
}

/// Split a command line into arguments, honoring double quotes so that
/// `argv = /bin/sh -c "echo hi there"` yields three arguments.
pub fn split_args(s: &str) -> std::result::Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut has_token = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quote = !in_quote;
                has_token = true;
            }
            '\\' if in_quote => {
                if let Some(&n) = chars.peek() {
                    cur.push(n);
                    chars.next();
                }
            }
            c if c.is_whitespace() && !in_quote => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if in_quote {
        return Err("unterminated quote in argv".into());
    }
    if has_token {
        out.push(cur);
    }
    Ok(out)
}

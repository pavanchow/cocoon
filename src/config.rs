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

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub hostname: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub isolate_net: bool,
    pub memory_max: Option<u64>,
    pub pids_max: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: "cocoon".into(),
            cwd: "/".into(),
            argv: Vec::new(),
            env: Vec::new(),
            isolate_net: false,
            memory_max: None,
            pids_max: None,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let mut c = Config::default();
        let mut saw_argv = false;
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
                "memory_max" => c.memory_max = Some(parse_u64(value, i + 1, "memory_max")?),
                "pids_max" => c.pids_max = Some(parse_u64(value, i + 1, "pids_max")?),
                other => {
                    return Err(Error::Config(format!(
                        "line {}: unknown key '{other}'",
                        i + 1
                    )))
                }
            }
        }
        if !saw_argv || c.argv.is_empty() {
            return Err(Error::Config("config must set a non-empty 'argv'".into()));
        }
        Ok(c)
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn parse_u64(v: &str, line: usize, key: &str) -> Result<u64> {
    v.parse::<u64>()
        .map_err(|_| Error::Config(format!("line {line}: {key} must be a non-negative integer")))
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

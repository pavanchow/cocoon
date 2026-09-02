//! A minimal Model Context Protocol server over stdio, so an agent can call the
//! sandbox directly. It speaks JSON-RPC 2.0 with newline-delimited messages (the
//! MCP stdio framing) and exposes one tool, `run_in_sandbox`, that runs a command
//! in the sandbox and returns the measured [`Outcome`](crate::Outcome) as JSON.
//!
//! The JSON-RPC plumbing is hand-rolled; only value parsing and serialization use
//! `serde_json`, kept to this one module.
use crate::config::{Config, Mount};
use crate::plan::Plan;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Read JSON-RPC messages from stdin, one per line, and write responses to
/// stdout. Requests (with an `id`) get a response; notifications (no `id`, e.g.
/// `notifications/initialized`) are acknowledged silently.
pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut out = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break; // EOF
        }
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => {
                write_msg(&mut out, &error(Value::Null, -32700, "parse error"))?;
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        // A message without an id is a notification: handle, do not respond.
        let id = match id {
            Some(id) if !id.is_null() => id,
            _ => continue,
        };
        let response = match method {
            "initialize" => ok(id, initialize_result()),
            "tools/list" => ok(id, json!({ "tools": [tool_schema()] })),
            "tools/call" => handle_call(id, msg.get("params")),
            "ping" => ok(id, json!({})),
            other => error(id, -32601, &format!("method not found: {other}")),
        };
        write_msg(&mut out, &response)?;
    }
    Ok(())
}

fn write_msg(out: &mut impl Write, v: &Value) -> std::io::Result<()> {
    out.write_all(v.to_string().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "cocoon", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn tool_schema() -> Value {
    json!({
        "name": "run_in_sandbox",
        "description": "Run a command inside a rootless, isolated, resource-limited sandbox and \
            return a structured result: exit_code, stdout, stderr, wall_ms, peak_mem_kib, \
            timed_out, oom_killed. Use this to execute untrusted or model-generated code safely. \
            Defaults to a cached minimal busybox root filesystem.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {
                    "description": "Either an argv array (e.g. [\"/bin/echo\",\"hi\"]) or a shell \
                        string, which is run with /bin/sh -c.",
                    "type": ["array", "string"],
                    "items": { "type": "string" }
                },
                "timeout": {
                    "description": "Wall-clock limit: a number of seconds, or a string like \
                        \"5s\", \"500ms\", \"2m\".",
                    "type": ["string", "number"]
                },
                "memory_max": {
                    "description": "Memory limit in bytes (best-effort; enforced only where a \
                        cgroup v2 subtree is delegated to the user).",
                    "type": "number"
                },
                "workdir": {
                    "description": "Absolute host directory to bind writable at /work, for passing \
                        inputs in and reading results back out.",
                    "type": "string"
                },
                "rootfs": {
                    "description": "Absolute path to a root filesystem directory to run against. \
                        Defaults to a cached minimal busybox rootfs.",
                    "type": "string"
                }
            },
            "required": ["command"]
        }
    })
}

fn handle_call(id: Value, params: Option<&Value>) -> Value {
    let params = match params {
        Some(p) => p,
        None => return error(id, -32602, "missing params"),
    };
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != "run_in_sandbox" {
        return error(id, -32602, &format!("unknown tool: {name}"));
    }
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    // A tool-level failure is reported as a result with isError=true, not a
    // JSON-RPC protocol error, per the MCP spec.
    let (text, is_error) = match run_tool(&args) {
        Ok(outcome_json) => (outcome_json, false),
        Err(e) => (e, true),
    };
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error
        }),
    )
}

/// Build a plan from the tool arguments and run it in the sandbox, returning the
/// Outcome JSON string, or a human-readable error string on failure.
fn run_tool(args: &Value) -> Result<String, String> {
    let argv: Vec<String> = match args.get("command") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or("`command` array must contain only strings")?,
        Some(Value::String(s)) => vec!["/bin/sh".into(), "-c".into(), s.clone()],
        _ => return Err("`command` must be an argv array or a shell string".into()),
    };
    if argv.is_empty() || argv[0].is_empty() {
        return Err("`command` is empty".into());
    }

    let mut cfg = Config {
        argv,
        ..Config::default()
    };
    if let Some(t) = args.get("timeout") {
        cfg.timeout_ms = Some(parse_timeout_value(t)?);
    }
    if let Some(m) = args.get("memory_max") {
        cfg.memory_max = Some(
            m.as_u64()
                .ok_or("`memory_max` must be a non-negative integer of bytes")?,
        );
    }
    if let Some(w) = args.get("workdir") {
        let w = w.as_str().ok_or("`workdir` must be a string")?;
        if !w.starts_with('/') {
            return Err("`workdir` must be an absolute path".into());
        }
        cfg.mounts.push(Mount {
            source: w.to_string(),
            target: "/work".into(),
            readonly: false,
        });
    }

    let rootfs = match args.get("rootfs") {
        Some(r) => {
            let r = r.as_str().ok_or("`rootfs` must be a string")?;
            std::path::PathBuf::from(r)
        }
        None => default_rootfs()?,
    };

    let plan = Plan::from_config(&cfg).map_err(|e| e.to_string())?;
    let outcome = crate::run_measured_plan(&plan, &rootfs).map_err(|e| e.to_string())?;
    Ok(outcome.to_json())
}

fn parse_timeout_value(t: &Value) -> Result<u64, String> {
    if let Some(n) = t.as_u64() {
        return n
            .checked_mul(1000)
            .ok_or_else(|| "`timeout` in seconds is too large".to_string());
    }
    if let Some(s) = t.as_str() {
        return crate::config::parse_timeout_ms(s).ok_or_else(|| format!("bad `timeout`: {s}"));
    }
    Err("`timeout` must be a number of seconds or a string like \"5s\"".into())
}

/// Path to the default busybox rootfs, building it under the user cache dir on
/// first use. Only meaningful on Linux, where the sandbox actually runs.
#[cfg(target_os = "linux")]
fn default_rootfs() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let root = cache.join("cocoon").join("rootfs");
    if root.join("bin").join("busybox").exists() {
        return Ok(root);
    }
    build_busybox_rootfs(&root).map_err(|e| format!("cannot prepare default rootfs: {e}"))?;
    Ok(root)
}

#[cfg(not(target_os = "linux"))]
fn default_rootfs() -> Result<std::path::PathBuf, String> {
    Err("the sandbox and its default busybox rootfs are only available on Linux".into())
}

#[cfg(target_os = "linux")]
fn build_busybox_rootfs(root: &std::path::Path) -> std::io::Result<()> {
    use std::path::Path;
    let bb = ["/usr/bin/busybox", "/bin/busybox"]
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "busybox not found on host; install busybox or pass an explicit rootfs",
            )
        })?;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::create_dir_all(root.join("proc"))?;
    std::fs::create_dir_all(root.join("work"))?;
    std::fs::copy(bb, bin.join("busybox"))?;
    for a in [
        "sh", "echo", "cat", "ls", "env", "id", "hostname", "sleep", "wc", "head", "grep", "touch",
        "true", "printf", "sed", "mkdir", "rm", "cut", "sort", "pwd", "date",
    ] {
        let _ = std::os::unix::fs::symlink("busybox", bin.join(a));
    }
    // Copy the dynamic loader and shared libraries busybox needs (none for a
    // static build). Best effort per library.
    if let Ok(out) = std::process::Command::new("ldd").arg(bb).output() {
        for tok in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            if tok.starts_with('/') && tok.contains(".so") {
                let src = Path::new(tok);
                if src.exists() {
                    let dst = root.join(src.strip_prefix("/").unwrap());
                    if let Some(p) = dst.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    let _ = std::fs::copy(src, &dst);
                }
            }
        }
    }
    Ok(())
}

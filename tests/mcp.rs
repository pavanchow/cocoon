//! Drives the `cocoon mcp` server by piping a scripted JSON-RPC exchange into it
//! and checking the responses. The handshake (initialize, tools/list) is checked
//! on every OS; the tool call actually runs the sandbox, so that part is
//! Linux-gated and skips when rootless user namespaces are unavailable.
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn drive(input: &str) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cocoon"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cocoon mcp");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each response line is valid JSON"))
        .collect()
}

#[test]
fn initialize_and_list_tools() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
    );
    let responses = drive(input);
    // The notification gets no response, so there are exactly two responses.
    assert_eq!(responses.len(), 2, "got: {responses:?}");

    let init = &responses[0];
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "cocoon");
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let list = &responses[1];
    assert_eq!(list["id"], 2);
    assert_eq!(list["result"]["tools"][0]["name"], "run_in_sandbox");
    assert!(list["result"]["tools"][0]["inputSchema"]["properties"]["command"].is_object());
}

#[test]
fn unknown_method_is_a_jsonrpc_error() {
    let responses = drive("{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"bogus\"}\n");
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["id"], 9);
    assert_eq!(responses[0]["error"]["code"], -32601);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{drive, Value};
    use std::path::Path;

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
        let _ = std::os::unix::fs::symlink("busybox", bin.join("echo"));
        if let Ok(out) = std::process::Command::new("ldd").arg(bb).output() {
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
    fn tool_call_runs_in_sandbox() {
        let base = std::env::temp_dir().join(format!("cocoon-mcp-{}", std::process::id()));
        let rootfs = base.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        if !build_rootfs(&rootfs) {
            eprintln!("busybox not found, skipping MCP tool-call test");
            return;
        }
        let call = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"run_in_sandbox","arguments":{{"command":["/bin/busybox","echo","mcp-ran"],"rootfs":"{}"}}}}}}"#,
            rootfs.display()
        );
        let responses = drive(&format!("{call}\n"));
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(responses.len(), 1);
        let result = &responses[0]["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        if result["isError"].as_bool().unwrap_or(false) {
            if text.contains("Operation not permitted")
                || text.contains("unshare")
                || text.contains("uid_map")
            {
                eprintln!("rootless user namespaces unavailable here, skipping: {text}");
                return;
            }
            panic!("tool call errored: {text}");
        }
        // The tool's text is the Outcome JSON string; parse and check it.
        let outcome: Value = serde_json::from_str(text).expect("Outcome JSON in tool result");
        assert_eq!(outcome["exit_code"], 0);
        assert_eq!(outcome["stdout"], "mcp-ran\n");
        assert_eq!(outcome["timed_out"], false);
    }
}

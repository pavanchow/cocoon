//! CLI: create a bundle skeleton, inspect the plan, or run a container.
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(|s| s.as_str()) {
        Some("run") => cmd_run(args.get(2)),
        Some("exec") => cmd_exec(&args[2..]),
        Some("mcp") => cmd_mcp(),
        Some("plan") => cmd_plan(args.get(2)),
        Some("spec") => cmd_spec(args.get(2)),
        Some("-h") | Some("--help") | None => {
            help();
            0
        }
        Some(other) => {
            eprintln!("cocoon: unknown command '{other}'");
            help();
            2
        }
    };
    std::process::exit(code);
}

fn help() {
    println!("cocoon: a small rootless Linux container runtime");
    println!();
    println!("  cocoon spec <dir>    create a bundle skeleton (cocoon.conf + rootfs/)");
    println!("  cocoon plan <dir>    parse the config and print what running it will do");
    println!("  cocoon run  <dir>    run the container to completion (Linux only)");
    println!("  cocoon exec <dir>    run it sandboxed and print a measured result");
    println!("       [--json]        as JSON: exit, stdout, stderr, wall_ms, peak_mem, timed_out");
    println!("       [--timeout N]   wall-clock limit, e.g. 5, 500ms, 2m (overrides the config)");
    println!("  cocoon mcp           serve the sandbox over MCP (JSON-RPC 2.0) on stdio");
    println!();
    println!("a bundle is a directory with a rootfs/ subdirectory and a cocoon.conf file.");
}

fn need_dir(dir: Option<&String>) -> Result<&Path, i32> {
    match dir {
        Some(d) => Ok(Path::new(d)),
        None => {
            eprintln!("cocoon: expected a bundle directory");
            Err(2)
        }
    }
}

fn cmd_run(dir: Option<&String>) -> i32 {
    let dir = match need_dir(dir) {
        Ok(d) => d,
        Err(c) => return c,
    };
    match cocoon::run(dir) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cocoon: {e}");
            1
        }
    }
}

fn cmd_exec(args: &[String]) -> i32 {
    let mut json = false;
    let mut dir: Option<&str> = None;
    let mut timeout: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--timeout" => {
                i += 1;
                timeout = args.get(i).map(|s| s.as_str());
            }
            s if !s.starts_with('-') => dir = Some(s),
            other => {
                eprintln!("cocoon: unknown flag '{other}'");
                return 2;
            }
        }
        i += 1;
    }
    let dir = match dir {
        Some(d) => Path::new(d),
        None => {
            eprintln!("cocoon: expected a bundle directory");
            return 2;
        }
    };
    let bundle = match cocoon::load_bundle(dir) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cocoon: {e}");
            return 1;
        }
    };
    let mut plan = match cocoon::Plan::from_config(&bundle.config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cocoon: {e}");
            return 1;
        }
    };
    if let Some(t) = timeout {
        match cocoon::config::parse_timeout_ms(t) {
            Some(ms) => plan.timeout_ms = Some(ms),
            None => {
                eprintln!("cocoon: bad --timeout '{t}' (try 5, 500ms, 2m)");
                return 2;
            }
        }
    }
    match cocoon::run_measured_plan(&plan, &bundle.rootfs) {
        Ok(o) => {
            if json {
                println!("{}", o.to_json());
            } else {
                print!("{}", o.stdout);
                eprint!("{}", o.stderr);
                eprintln!(
                    "--- exit {} · {} ms{}{}{} ---",
                    o.exit_code,
                    o.wall_ms,
                    o.peak_mem_kib
                        .map(|k| format!(" · peak {k} KiB"))
                        .unwrap_or_default(),
                    if o.timed_out { " · TIMED OUT" } else { "" },
                    if o.oom_killed { " · OOM" } else { "" }
                );
            }
            o.exit_code
        }
        Err(e) => {
            eprintln!("cocoon: {e}");
            1
        }
    }
}

fn cmd_mcp() -> i32 {
    match cocoon::mcp::serve() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cocoon: mcp: {e}");
            1
        }
    }
}

fn cmd_plan(dir: Option<&String>) -> i32 {
    let dir = match need_dir(dir) {
        Ok(d) => d,
        Err(c) => return c,
    };
    match cocoon::plan_bundle(dir) {
        Ok(plan) => {
            print!("{}", plan.describe());
            0
        }
        Err(e) => {
            eprintln!("cocoon: {e}");
            1
        }
    }
}

fn cmd_spec(dir: Option<&String>) -> i32 {
    let dir = match need_dir(dir) {
        Ok(d) => d,
        Err(c) => return c,
    };
    if let Err(e) = std::fs::create_dir_all(dir.join("rootfs")) {
        eprintln!("cocoon: cannot create {}: {e}", dir.display());
        return 1;
    }
    let conf = dir.join("cocoon.conf");
    if let Err(e) = std::fs::write(&conf, cocoon::DEFAULT_CONFIG) {
        eprintln!("cocoon: cannot write {}: {e}", conf.display());
        return 1;
    }
    println!("wrote {}", conf.display());
    println!(
        "populate {}/rootfs with a root filesystem, then: cocoon run {}",
        dir.display(),
        dir.display()
    );
    0
}

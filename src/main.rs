//! CLI: create a bundle skeleton, inspect the plan, or run a container.
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(|s| s.as_str()) {
        Some("run") => cmd_run(args.get(2)),
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

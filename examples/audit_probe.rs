use cocoon::config::{split_args, Config};
use cocoon::plan::Plan;

fn show(label: &str, text: &str) {
    print!("=== {label}\ninput: {text:?}\n");
    match Config::parse(text) {
        Ok(c) => println!(
            "OK  argv={:?} env={:?} mem={:?} pids={:?}",
            c.argv, c.env, c.memory_max, c.pids_max
        ),
        Err(e) => println!("ERR {e}"),
    }
}

fn main() {
    // 1. '#' inside a quoted argv value
    show("hash-in-quoted-argv", "argv = /bin/echo \"a#b\"");
    // 2. '#' inside env value
    show("hash-in-env-value", "argv = /bin/sh\nenv = MSG=hello#world");
    // 3. memory_max overflow (> u64::MAX)
    show(
        "mem-overflow",
        "argv=/bin/sh\nmemory_max = 99999999999999999999999999",
    );
    // 4. leading + on integer
    show("mem-plus-prefix", "argv=/bin/sh\nmemory_max = +5");
    // 5. empty quoted argv -> single empty argument
    show("empty-quoted-argv", "argv = \"\"");
    // 6. env value with '='
    show("env-eq-in-value", "argv=/bin/sh\nenv = TOKEN=a=b=c");
    // 7. duplicate hostname
    show("dup-hostname", "argv=/bin/sh\nhostname = a\nhostname = b");
    // 8. NUL byte in argv -> would panic at CString::new in child; show parse succeeds
    show("nul-in-argv", "argv = /bin/sh\0evil");

    // empty-arg plan + would-be exec target
    if let Ok(c) = Config::parse("argv = \"\"") {
        if let Ok(p) = Plan::from_config(&c) {
            println!(
                "empty-quoted-argv PLAN argv={:?} (argv[0]={:?})",
                p.argv,
                p.argv.get(0)
            );
        }
    }

    // direct split_args probes
    println!("split_args(\"\\\"\\\"\") = {:?}", split_args("\"\""));
    println!("split_args(a#b as seen after strip) not applicable (strip happens before)");
}

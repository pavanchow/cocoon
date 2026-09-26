//! Integration tests for the OS-independent core: config parsing, plan
//! derivation, and JSON output. These exercise only the public API and run on
//! any platform, since none of them touch Linux namespaces.
use cocoon::config::{split_args, Config};
use cocoon::plan::{Limits, Namespace, Plan};
use cocoon::{Outcome, DEFAULT_CONFIG};

fn parse(text: &str) -> Config {
    Config::parse(text).expect("config should parse")
}

// --- config: basics ---------------------------------------------------------

#[test]
fn minimal_config_needs_only_argv() {
    let c = parse("argv = /bin/true");
    assert_eq!(c.argv, vec!["/bin/true"]);
    // Untouched fields keep their defaults.
    assert_eq!(c.hostname, "cocoon");
    assert_eq!(c.cwd, "/");
    assert!(!c.isolate_net);
    assert!(!c.readonly);
    assert!(c.env.is_empty());
    assert!(c.mounts.is_empty());
}

#[test]
fn missing_argv_is_an_error() {
    assert!(Config::parse("hostname = box").is_err());
}

#[test]
fn empty_argv_value_is_an_error() {
    assert!(Config::parse("argv =").is_err());
}

#[test]
fn all_scalar_fields_parse() {
    let c = parse(
        "hostname = sandbox\n\
         cwd = /work\n\
         argv = /bin/echo hi\n\
         net = isolated\n\
         readonly = true\n\
         memory_max = 67108864\n\
         pids_max = 64\n\
         timeout = 5s\n",
    );
    assert_eq!(c.hostname, "sandbox");
    assert_eq!(c.cwd, "/work");
    assert_eq!(c.argv, vec!["/bin/echo", "hi"]);
    assert!(c.isolate_net);
    assert!(c.readonly);
    assert_eq!(c.memory_max, Some(67108864));
    assert_eq!(c.pids_max, Some(64));
    assert_eq!(c.timeout_ms, Some(5000));
}

#[test]
fn env_may_repeat_and_keeps_order() {
    let c = parse(
        "argv = /bin/sh\n\
         env = PATH=/usr/bin:/bin\n\
         env = HOME=/root\n",
    );
    assert_eq!(
        c.env,
        vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("HOME".to_string(), "/root".to_string()),
        ]
    );
}

#[test]
fn env_value_may_contain_equals_signs() {
    let c = parse("argv = /bin/sh\nenv = FLAGS=a=b=c\n");
    assert_eq!(c.env, vec![("FLAGS".to_string(), "a=b=c".to_string())]);
}

// --- config: errors ----------------------------------------------------------

#[test]
fn unknown_key_is_rejected() {
    let err = Config::parse("argv = /bin/sh\nfrobnicate = yes\n").unwrap_err();
    assert!(format!("{err}").contains("unknown key"));
}

#[test]
fn line_without_equals_is_rejected() {
    assert!(Config::parse("argv = /bin/sh\njust some words\n").is_err());
}

#[test]
fn net_only_accepts_host_or_isolated() {
    assert!(Config::parse("argv = /bin/sh\nnet = maybe\n").is_err());
    assert!(!parse("argv = /bin/sh\nnet = host\n").isolate_net);
    assert!(parse("argv = /bin/sh\nnet = isolated\n").isolate_net);
}

#[test]
fn readonly_only_accepts_true_or_false() {
    assert!(Config::parse("argv = /bin/sh\nreadonly = yes\n").is_err());
}

#[test]
fn memory_max_must_be_an_integer() {
    assert!(Config::parse("argv = /bin/sh\nmemory_max = lots\n").is_err());
}

#[test]
fn error_message_reports_the_line_number() {
    let err = Config::parse("argv = /bin/sh\n\nnet = bogus\n").unwrap_err();
    assert!(format!("{err}").contains("line 3"), "got: {err}");
}

// --- config: comments and blank lines ----------------------------------------

#[test]
fn comments_and_blank_lines_are_ignored() {
    let c = parse(
        "# a leading comment\n\
         \n\
         argv = /bin/sh   # trailing comment\n\
         # another\n",
    );
    assert_eq!(c.argv, vec!["/bin/sh"]);
}

#[test]
fn hash_inside_quotes_is_kept() {
    let c = parse("argv = /bin/sh -c \"echo #1\"\n");
    assert_eq!(c.argv, vec!["/bin/sh", "-c", "echo #1"]);
}

#[test]
fn unspaced_hash_in_a_value_is_not_a_comment() {
    let c = parse("argv = /bin/sh\nhostname = a#b\n");
    assert_eq!(c.hostname, "a#b");
}

// --- config: mounts ----------------------------------------------------------

#[test]
fn mounts_parse_and_repeat() {
    let c = parse(
        "argv = /bin/sh\n\
         mount = /host/in:/in:ro\n\
         mount = /host/out:/work:rw\n",
    );
    assert_eq!(c.mounts.len(), 2);
    assert_eq!(c.mounts[0].source, "/host/in");
    assert_eq!(c.mounts[0].target, "/in");
    assert!(c.mounts[0].readonly);
    assert!(!c.mounts[1].readonly);
}

#[test]
fn mount_requires_absolute_paths() {
    assert!(Config::parse("argv = /bin/sh\nmount = rel:/work:rw\n").is_err());
    assert!(Config::parse("argv = /bin/sh\nmount = /host:rel:rw\n").is_err());
}

#[test]
fn mount_flag_must_be_ro_or_rw() {
    assert!(Config::parse("argv = /bin/sh\nmount = /a:/b:rx\n").is_err());
}

#[test]
fn mount_needs_exactly_three_fields() {
    assert!(Config::parse("argv = /bin/sh\nmount = /a:/b\n").is_err());
}

// --- config: profiles ---------------------------------------------------------

#[test]
fn strict_profile_locks_down_defaults() {
    let c = parse("argv = /bin/sh\nprofile = strict\n");
    assert!(c.isolate_net);
    assert!(c.readonly);
    assert_eq!(c.timeout_ms, Some(5_000));
    assert_eq!(c.memory_max, Some(128 * 1024 * 1024));
}

#[test]
fn build_profile_loosens_defaults() {
    let c = parse("argv = /bin/sh\nprofile = build\n");
    assert!(c.isolate_net);
    assert!(!c.readonly);
    assert_eq!(c.timeout_ms, Some(300_000));
    assert_eq!(c.memory_max, Some(1024 * 1024 * 1024));
}

#[test]
fn explicit_key_overrides_profile_regardless_of_order() {
    // `net = host` appears before the profile line and must still win.
    let c = parse("argv = /bin/sh\nnet = host\nprofile = strict\n");
    assert!(!c.isolate_net, "explicit net=host must beat profile strict");
    // readonly was not set explicitly, so the profile fills it.
    assert!(c.readonly);
}

#[test]
fn unknown_profile_is_rejected() {
    assert!(Config::parse("argv = /bin/sh\nprofile = paranoid\n").is_err());
}

// --- split_args --------------------------------------------------------------

#[test]
fn split_args_honors_quotes() {
    assert_eq!(
        split_args("/bin/sh -c \"echo hi there\"").unwrap(),
        vec!["/bin/sh", "-c", "echo hi there"]
    );
}

#[test]
fn split_args_handles_escaped_quote_inside_quotes() {
    assert_eq!(
        split_args("say \"a \\\"b\\\" c\"").unwrap(),
        vec!["say", "a \"b\" c"]
    );
}

#[test]
fn split_args_keeps_empty_quoted_token() {
    assert_eq!(split_args("cmd \"\"").unwrap(), vec!["cmd", ""]);
}

#[test]
fn split_args_rejects_unterminated_quote() {
    assert!(split_args("/bin/sh -c \"oops").is_err());
}

// --- plan derivation ----------------------------------------------------------

#[test]
fn plan_has_the_five_base_namespaces_without_net() {
    let plan = Plan::from_config(&parse("argv = /bin/sh")).unwrap();
    assert_eq!(
        plan.namespaces,
        vec![
            Namespace::User,
            Namespace::Mount,
            Namespace::Pid,
            Namespace::Uts,
            Namespace::Ipc,
        ]
    );
}

#[test]
fn isolated_net_adds_the_net_namespace() {
    let plan = Plan::from_config(&parse("argv = /bin/sh\nnet = isolated\n")).unwrap();
    assert!(plan.namespaces.contains(&Namespace::Net));
    assert_eq!(plan.namespaces.len(), 6);
}

#[test]
fn plan_rejects_empty_argv() {
    // A default Config has no argv; the plan step must refuse it.
    assert!(Plan::from_config(&Config::default()).is_err());
}

#[test]
fn plan_rejects_relative_cwd() {
    let mut cfg = Config::default();
    cfg.argv = vec!["/bin/sh".into()];
    cfg.cwd = "relative".into();
    assert!(Plan::from_config(&cfg).is_err());
}

#[test]
fn plan_carries_limits_and_mounts_through() {
    let plan = Plan::from_config(&parse(
        "argv = /bin/sh\n\
         memory_max = 4096\n\
         pids_max = 8\n\
         mount = /a:/b:ro\n",
    ))
    .unwrap();
    assert_eq!(plan.limits, Limits { memory_max: Some(4096), pids_max: Some(8) });
    assert!(plan.limits.any());
    assert_eq!(plan.mounts.len(), 1);
}

#[test]
fn limits_any_is_false_when_unset() {
    let plan = Plan::from_config(&parse("argv = /bin/sh")).unwrap();
    assert!(!plan.limits.any());
}

#[test]
fn describe_reports_namespaces_and_hardening() {
    let plan = Plan::from_config(&parse("argv = /bin/sh\nnet = isolated\n")).unwrap();
    let text = plan.describe();
    assert!(text.contains("user, mount, pid, uts, ipc, net"));
    assert!(text.contains("hardening"));
    assert!(text.contains("no_new_privs"));
}

// --- default config round-trip -----------------------------------------------

#[test]
fn default_config_parses_and_plans() {
    let cfg = Config::parse(DEFAULT_CONFIG).expect("shipped default must parse");
    assert_eq!(cfg.argv, vec!["/bin/sh"]);
    // Every commented line in the default must be ignored, not error.
    Plan::from_config(&cfg).expect("shipped default must produce a plan");
}

// --- Outcome JSON -------------------------------------------------------------

#[test]
fn outcome_json_has_all_fields() {
    let o = Outcome {
        exit_code: 0,
        timed_out: false,
        oom_killed: false,
        wall_ms: 12,
        peak_mem_kib: Some(2048),
        stdout: "ok".into(),
        stderr: String::new(),
    };
    let j = o.to_json();
    assert!(j.contains("\"exit_code\":0"));
    assert!(j.contains("\"wall_ms\":12"));
    assert!(j.contains("\"peak_mem_kib\":2048"));
    assert!(j.contains("\"stdout\":\"ok\""));
}

#[test]
fn outcome_json_escapes_control_characters() {
    let o = Outcome {
        exit_code: 1,
        timed_out: true,
        oom_killed: false,
        wall_ms: 0,
        peak_mem_kib: None,
        stdout: "line1\nline2\t\"q\"".into(),
        stderr: String::new(),
    };
    let j = o.to_json();
    assert!(j.contains("\\n"), "newline must be escaped: {j}");
    assert!(j.contains("\\t"), "tab must be escaped");
    assert!(j.contains("\\\""), "quote must be escaped");
    assert!(j.contains("\"peak_mem_kib\":null"), "None becomes null");
    assert!(j.contains("\"timed_out\":true"));
}

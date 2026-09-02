use cocoon::config::{split_args, Config};
use cocoon::plan::{Namespace, Plan};
use cocoon::{Container, Error, State};

// ---- config parsing ----

#[test]
fn parses_minimal_config() {
    let c = Config::parse("argv = /bin/sh").unwrap();
    assert_eq!(c.argv, vec!["/bin/sh"]);
    assert_eq!(c.hostname, "cocoon");
    assert_eq!(c.cwd, "/");
    assert!(!c.isolate_net);
}

#[test]
fn parses_full_config() {
    let text = "\
# a bundle
hostname = webbox
cwd = /app
argv = /bin/sh -c \"echo hi there\"
env = PATH=/usr/bin:/bin
env = TERM=xterm
net = isolated
memory_max = 67108864
pids_max = 64
";
    let c = Config::parse(text).unwrap();
    assert_eq!(c.hostname, "webbox");
    assert_eq!(c.cwd, "/app");
    assert_eq!(c.argv, vec!["/bin/sh", "-c", "echo hi there"]);
    assert_eq!(
        c.env,
        vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TERM".to_string(), "xterm".to_string()),
        ]
    );
    assert!(c.isolate_net);
    assert_eq!(c.memory_max, Some(67108864));
    assert_eq!(c.pids_max, Some(64));
}

#[test]
fn rejects_missing_argv() {
    assert!(matches!(
        Config::parse("hostname = x"),
        Err(Error::Config(_))
    ));
}

#[test]
fn rejects_unknown_key() {
    let e = Config::parse("argv=/bin/sh\nbogus = 1").unwrap_err();
    assert!(format!("{e}").contains("unknown key 'bogus'"));
}

#[test]
fn rejects_bad_net() {
    let e = Config::parse("argv=/bin/sh\nnet = maybe").unwrap_err();
    assert!(format!("{e}").contains("net must be"));
}

#[test]
fn split_args_honors_quotes() {
    assert_eq!(split_args("a b c").unwrap(), vec!["a", "b", "c"]);
    assert_eq!(
        split_args("/bin/sh -c \"echo a b\"").unwrap(),
        vec!["/bin/sh", "-c", "echo a b"]
    );
    assert_eq!(
        split_args("  spaced   out  ").unwrap(),
        vec!["spaced", "out"]
    );
    assert!(split_args("oops \"unterminated").is_err());
}

#[test]
fn hash_is_a_comment_only_outside_quotes_and_after_space() {
    // '#' inside a quoted argv is kept, not treated as a comment
    let c = Config::parse("argv = /bin/sh -c \"echo #1 done\"").unwrap();
    assert_eq!(c.argv, vec!["/bin/sh", "-c", "echo #1 done"]);
    // '#' with no preceding space in a value is part of the value
    let c2 = Config::parse("argv=/bin/sh\nenv = MSG=a#b").unwrap();
    assert_eq!(c2.env, vec![("MSG".to_string(), "a#b".to_string())]);
    // a spaced '#' is still an inline comment
    let c3 = Config::parse("hostname = box   # a note\nargv = /bin/sh").unwrap();
    assert_eq!(c3.hostname, "box");
    // a full-line comment
    let c4 = Config::parse("# just a comment\nargv = /bin/sh").unwrap();
    assert_eq!(c4.argv, vec!["/bin/sh"]);
}

// ---- plan ----

#[test]
fn plan_default_namespaces_are_rootless() {
    let c = Config::parse("argv = /bin/sh").unwrap();
    let p = Plan::from_config(&c).unwrap();
    assert_eq!(
        p.namespaces,
        vec![
            Namespace::User,
            Namespace::Mount,
            Namespace::Pid,
            Namespace::Uts,
            Namespace::Ipc
        ]
    );
    assert!(!p.namespaces.contains(&Namespace::Net));
    assert!(
        p.namespaces.contains(&Namespace::User),
        "rootless always uses a user namespace"
    );
}

#[test]
fn plan_adds_net_when_isolated() {
    let c = Config::parse("argv = /bin/sh\nnet = isolated").unwrap();
    let p = Plan::from_config(&c).unwrap();
    assert!(p.namespaces.contains(&Namespace::Net));
}

#[test]
fn plan_rejects_relative_cwd() {
    let mut c = Config::parse("argv = /bin/sh").unwrap();
    c.cwd = "relative".into();
    assert!(matches!(Plan::from_config(&c), Err(Error::Plan(_))));
}

#[test]
fn plan_describe_mentions_key_fields() {
    let c = Config::parse("hostname = box\nargv = /bin/echo hi").unwrap();
    let d = Plan::from_config(&c).unwrap().describe();
    assert!(d.contains("box"));
    assert!(d.contains("/bin/echo hi"));
    assert!(d.contains("user, mount, pid, uts, ipc"));
}

// ---- lifecycle state machine ----

#[test]
fn lifecycle_happy_path() {
    let mut ct = Container::create("c1");
    assert_eq!(ct.state, State::Created);
    ct.start(1234).unwrap();
    assert!(ct.is_running());
    ct.stop(0).unwrap();
    assert_eq!(ct.exit_code(), Some(0));
}

#[test]
fn cannot_stop_before_start() {
    let mut ct = Container::create("c1");
    assert!(ct.stop(0).is_err());
}

#[test]
fn cannot_start_twice() {
    let mut ct = Container::create("c1");
    ct.start(1).unwrap();
    assert!(ct.start(2).is_err());
}

#[test]
fn cannot_start_after_stop() {
    let mut ct = Container::create("c1");
    ct.start(1).unwrap();
    ct.stop(0).unwrap();
    assert!(ct.start(2).is_err());
}

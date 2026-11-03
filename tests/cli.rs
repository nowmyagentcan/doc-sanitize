use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_doc-sanitize"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn version_prints_the_name_and_version() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "doc-sanitize 0.1.0\n");
    }
}

#[test]
fn no_arguments_is_a_usage_error() {
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("No file given."));
}

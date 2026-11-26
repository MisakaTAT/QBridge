use std::process::Command;

fn qbridge(argument: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_qbridge"))
        .arg(argument)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn prints_the_version() {
    let expected = format!("qbridge {}\n", qbridge::VERSION);
    assert_eq!(qbridge("--version"), expected);
    assert_eq!(qbridge("-V"), expected);
}

#[test]
fn local_builds_are_marked_as_dev_unless_a_release_version_is_injected() {
    match option_env!("QBRIDGE_VERSION") {
        Some(version) if !version.is_empty() => assert_eq!(qbridge::VERSION, version),
        _ => assert!(qbridge::VERSION.ends_with("-dev")),
    }
}

#[test]
fn help_lists_the_version_flag() {
    assert!(qbridge("--help").contains("[--version]"));
}

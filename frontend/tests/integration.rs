extern crate executable_path;
extern crate tempfile;

use std::process;
use std::str;
use std::fs::File;
use std::io::Read;

use executable_path::executable_path;

fn integration_test(cmd: &str, status: i32, stdout: &[u8], stderr: &[u8]) {
    println!("testing command {}", cmd);
    let output = process::Command::new(&executable_path("frontend"))
        .args(&[
            "--command", &cmd,
            "--backend", executable_path("backend").to_str().unwrap()
        ])
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("frontend invocation failed");

    assert_eq!(str::from_utf8(&output.stderr).unwrap(), str::from_utf8(stderr).unwrap());
    assert_eq!(str::from_utf8(&output.stdout).unwrap(), str::from_utf8(stdout).unwrap());
    assert_eq!(output.status.code(), Some(status));
}

#[derive(Debug)]
enum Cmd {
    StdoutLine,
    StderrLine,
}

fn integration_test_with_test_helper(cmds: &[Cmd], status: i32, stdout: &[u8], stderr: &[u8]) {
    let mut cmd = format!("{}", executable_path("test_helper").display());
    for c in cmds {
        cmd.push_str(&format!(" {:?}", c));
    }
    integration_test(&cmd, status, stdout, stderr);
}

#[test]
fn echo_cmd() {
    integration_test(
        "echo test1234test",
        0,
        b"test1234test\n",
        b"");
}

#[test]
fn test_stdout() {
    integration_test_with_test_helper(&[Cmd::StdoutLine], 0, b"test1234teststdout\n", b"");
}

#[test]
fn test_stderr() {
    integration_test_with_test_helper(&[Cmd::StderrLine], 0, b"", b"test1234teststderr\n");
}

#[test]
fn test_stdout_stderr() {
    integration_test_with_test_helper(&[Cmd::StdoutLine, Cmd::StderrLine],
                                      0,
                                      b"test1234teststdout\n",
                                      b"test1234teststderr\n");
}

#[test]
fn redirect_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let file = temp.path().join("test_file");

    let cmd = format!("{} StdoutLine > {}",
        executable_path("test_helper").display(),
        file.display());
    integration_test(&cmd, 0, b"", b"");

    let mut contents = Vec::new();
    File::open(file).unwrap().read_to_end(&mut contents).unwrap();

    assert_eq!(contents, b"test1234teststdout\n");
}
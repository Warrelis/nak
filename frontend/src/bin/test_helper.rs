use std::io;
use std::env;
use std::io::Write;
use std::time::Duration;
use std::thread;

fn main() {
    let args: Vec<String> = env::args().collect();

    assert!(args.len() > 1);

    for a in &args[1..] {
        match a.as_str() {
            "StdoutLine" => {
                let mut stdout = io::stdout();
                stdout.write_all(b"test1234teststdout\n").unwrap();
                stdout.flush().unwrap();
            }
            "StderrLine" => {
                let mut stderr = io::stderr();
                stderr.write_all(b"test1234teststderr\n").unwrap();
                stderr.flush().unwrap();
            }
            "Wait" => thread::sleep(Duration::from_millis(100)),
            _ => panic!("unknown command {}", a)
        }
    }
}
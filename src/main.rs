use std::io::Write;
use std::io;
use std::process as pr;

pub enum Tool {
    Path(String),
}

pub struct Args {
    args: Vec<String>
}

pub struct Command {
    tool: Tool,
    args: Args,
}

#[derive(Debug)]
pub enum Response {
    Text(String),
}

pub trait Remote {
    fn run(&self, c: Command) -> Response;
}

pub struct SimpleRemote;

impl Remote for SimpleRemote {
    fn run(&self, c: Command) -> Response {
        match c.tool {
            Tool::Path(path) => {
                let out = pr::Command::new(path)
                    .args(&c.args.args)
                    .output()
                    .expect("failed to execute process");

                Response::Text(String::from_utf8(out.stdout).unwrap())
            }
        }
    }
}

trait Reader {
    fn get_command(&mut self, remote: &dyn Remote) -> Command;
}

fn parse_command_simple(input: &str) -> Command {
    let parts = input.trim().split(" ");
    let mut c = None;
    let mut args = Vec::new();
    for p in parts {
        if c.is_none() {
            c = Some(p.to_string());
        } else {
            args.push(p.to_string());
        }
    }
    Command {
        tool: Tool::Path(c.unwrap()),
        args: Args { args },
    }
}

struct SimpleReader;

impl Reader for SimpleReader {
    fn get_command(&mut self, _: &dyn Remote) -> Command {
        let mut input = String::new();

        print!("> ");
        io::stdout().flush().unwrap();
        match io::stdin().read_line(&mut input) {
            Ok(_) => parse_command_simple(&input),
            Err(error) => panic!("error: {}", error),
        }
    }
}

fn main() {
    let mut reader = SimpleReader;
    let remote = SimpleRemote;

    loop {
        let cmd = reader.get_command(&remote);

        let res = remote.run(cmd);

        match res {
            Response::Text(text) => println!("{}", text),
        }
    }
}

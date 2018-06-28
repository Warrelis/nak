#[macro_use]
extern crate failure;

use std::io::Write;
use std::io;
use std::process as pr;

use failure::Error;

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
    fn run(&self, c: Command) -> Result<Response, Error>;
}

pub struct SimpleRemote;

impl Remote for SimpleRemote {
    fn run(&self, c: Command) -> Result<Response, Error> {
        match c.tool {
            Tool::Path(path) => {
                let out = pr::Command::new(path)
                    .args(&c.args.args)
                    .output()?;

                Ok(Response::Text(String::from_utf8(out.stdout)?))
            }
        }
    }
}

trait Reader {
    fn get_command(&mut self, remote: &dyn Remote) -> Result<Command, Error>;
}

fn parse_command_simple(input: &str) -> Result<Command, Error> {
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
    Ok(Command {
        tool: Tool::Path(c.ok_or_else(|| format_err!("No binary"))?),
        args: Args { args },
    })
}

struct SimpleReader;

impl Reader for SimpleReader {
    fn get_command(&mut self, _: &dyn Remote) -> Result<Command, Error> {
        let mut input = String::new();

        print!("> ");
        io::stdout().flush().unwrap();
        Ok(io::stdin().read_line(&mut input)
            .map_err(Error::from)
            .and_then(|_| parse_command_simple(&input))?)
    }
}

fn main() {
    let mut reader = SimpleReader;
    let remote = SimpleRemote;

    loop {
        let e = reader.get_command(&remote)
            .and_then(|cmd| remote.run(cmd))
            .map(|res| match res {
                Response::Text(text) => println!("{}", text),
            }).err();

        if let Some(e) = e {
            println!("{:?}", e);
        }
    }
}

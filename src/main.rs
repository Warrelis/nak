#[macro_use]
extern crate failure;

extern crate futures;

use std::io::Write;
use std::io;
use std::process as pr;

use failure::Error;
use futures::{Future, Stream, Async};
use futures::task::Context;
use futures::executor::ThreadPool;

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

pub enum Response {
    Text(String),
    Stream(Box<Stream<Item=String, Error=Error>>)
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

struct WritingFuture(Box<dyn Stream<Item=String, Error=Error>>);

impl Future for WritingFuture {
    type Item = ();
    type Error = Error;

    fn poll(&mut self, cx: &mut Context) -> Result<Async<Self::Item>, Self::Error> {
        match self.0.poll_next(cx) {
            Ok(Async::Pending) => panic!(),
            Ok(Async::Ready(Some(_val))) => panic!(),
            Ok(Async::Ready(None)) => panic!(),
            Err(_err) => panic!(),
        }
    }
}

fn run_to_completion(s: Box<dyn Stream<Item=String, Error=Error>>) -> impl Future<Item=(), Error=Error> {

    WritingFuture(s)
}

fn main() {
    let mut reader = SimpleReader;
    let remote = SimpleRemote;

    loop {
        let e = reader.get_command(&remote)
            .and_then(|cmd| remote.run(cmd))
            .and_then(|res| match res {
                Response::Text(text) => {
                    println!("{}", text);
                    Ok(())
                }
                Response::Stream(stream) => {
                    ThreadPool::new()?
                        .run(run_to_completion(stream))
                }
            }).err();

        if let Some(e) = e {
            println!("{:?}", e);
        }
    }
}

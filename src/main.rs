#[macro_use]
extern crate failure;
extern crate os_pipe;
extern crate futures;
extern crate tokio;
extern crate base64;
extern crate liner;

use std::borrow::Borrow;
use os_pipe::IntoStdio;
use std::io::Read;
use std::str;
use std::io;
use std::process as pr;

use failure::Error;
use futures::{Future, Stream, Async};
use futures::task::Context;
use futures::executor::ThreadPool;

mod parse;

use parse::Parser;

#[derive(Clone, Debug, PartialEq)]
pub enum Tool {
    Path(String),
    Remote {
        host: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    args: Vec<String>
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    tool: Tool,
    args: Args,
}

pub enum Response {
    Text(String),
    Stream {
        stdout: Box<Stream<Item=String, Error=Error>>,
    },
    Remote(Box<dyn Remote>),
}

struct SingleStream {
    out: Option<String>
}

impl Stream for SingleStream {
    type Item = String;
    type Error = Error;

    fn poll_next(
        &mut self,
        _cx: &mut Context
    ) -> Result<Async<Option<Self::Item>>, Self::Error> {
        match self.out.take() {
            Some(out) => Ok(Async::Ready(Some(out))),
            None => Ok(Async::Ready(None))
        }
    }
}

pub trait Remote {
    fn run(&mut self, c: Command) -> Result<Response, Error>;
}

pub struct SimpleRemote;

impl Remote for SimpleRemote {
    fn run(&mut self, c: Command) -> Result<Response,
 Error> {
        match c.tool {
            Tool::Path(path) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&c.args.args);

                let (mut reader, writer) = os_pipe::pipe()?;

                cmd.stdout(writer.into_stdio());

                let mut handle = cmd.spawn()?;

                drop(cmd);

                let mut output = String::new();

                reader.read_to_string(&mut output)?;
                handle.wait()?;

                Ok(Response::Stream {
                    stdout: Box::new(SingleStream { out: Some(output) })
                })
            }
            Tool::Remote { .. } => panic!(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Shell {
    DoNothing,
    Run(Command),
    Exit,
}

trait Reader {
    fn get_command(&mut self, remote: &dyn Remote) -> Result<Shell, Error>;
}

fn parse_command_simple(input: &str) -> Result<Shell, Error> {
    let mut p = Parser::new();

    if input.len() == 0 {
        return Ok(Shell::DoNothing);
    }

    let cmd = p.parse(input);

    Ok(Shell::Run(Command {
        tool: Tool::Path(cmd.head().to_string()),
        args: Args { args: cmd.body().into_iter().map(String::from).collect() },
    }))
}

struct SimpleReader {
    ctx: liner::Context,
}

impl SimpleReader {
    fn new() -> SimpleReader {
        SimpleReader {
            ctx: liner::Context::new(),
        }
    }
}

impl Reader for SimpleReader {
    fn get_command(&mut self, _: &dyn Remote) -> Result<Shell, Error> {
        let res = match self.ctx.read_line("[prompt]$ ", &mut |_| {}) {
            Ok(res) => res,
            Err(e) => {
                return match e.kind() {
                    io::ErrorKind::Interrupted => Ok(Shell::DoNothing),
                    io::ErrorKind::UnexpectedEof => Ok(Shell::Exit),
                    _ => Err(e.into()),
                }
            }
        };

        Ok(parse_command_simple(&res)?)
    }
}

struct WritingFuture(Box<dyn Stream<Item=String, Error=Error>>);

impl Future for WritingFuture {
    type Item = ();
    type Error = Error;

    fn poll(&mut self, cx: &mut Context)
        -> Result<Async<Self::Item>, Self::Error>
    {
        loop {
            match self.0.poll_next(cx) {
                Ok(Async::Ready(Some(val))) => print!("{}", val),
                Ok(Async::Pending) => return Ok(Async::Pending),
                Ok(Async::Ready(None)) => return Ok(Async::Ready(())),
                Err(err) => return Err(err),
            }
        }
    }
}

#[test]
fn parse_simple() {
    let c = parse_command_simple(" test 1 abc 2").unwrap();
    assert_eq!(c, Shell::Run(Command {
        tool: Tool::Path(String::from("test")),
        args: Args {args: vec![
            String::from("1"),
            String::from("abc"),
            String::from("2"),
        ]}
    }));
}

fn run_to_completion(s: Box<dyn Stream<Item=String, Error=Error>>)
    -> impl Future<Item=(), Error=Error>
{
    WritingFuture(s)
}

fn one_loop(remote: &mut dyn Remote, reader: &mut dyn Reader)
    -> Result<bool, Error>
{
    match reader.get_command(remote.borrow())? {
        Shell::Exit => return Ok(false),
        Shell::DoNothing => {}
        Shell::Run(cmd) => {
            let res = remote.run(cmd)?;
        
            match res {
                Response::Text(text) => {
                    println!("{}", text);
                }
                Response::Stream { stdout } => {
                    ThreadPool::new()?
                        .run(run_to_completion(stdout))?;
                }
                Response::Remote(remote) => {
                    remote_run(remote, reader)?;
                }
            }
        }
    }
    Ok(true)
}

fn remote_run(mut remote: Box<dyn Remote>, reader: &mut dyn Reader)
    -> Result<(), Error>
{
    while one_loop(remote.as_mut(), reader)? {}
    Ok(())
}

fn main() {
    let mut reader = SimpleReader::new();
    let remote = Box::new(SimpleRemote);

    remote_run(remote, &mut reader).unwrap();
}

// #[macro_use]
extern crate failure;
extern crate os_pipe;
extern crate futures;
extern crate tokio;
extern crate rand;
extern crate base64;

use rand::thread_rng;
use rand::Rng;
use os_pipe::IntoStdio;
use std::io::Read;
use std::io::Write;
use std::io;
use std::str;
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
    }
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
    fn run(&mut self, c: Command) -> Result<Response, Error> {
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
        }
    }
}

struct ShRemote {
    input: os_pipe::PipeWriter,
    output: os_pipe::PipeReader,
    error: os_pipe::PipeReader,
    child: pr::Child,
}

impl ShRemote {
    fn new() -> Result<ShRemote, Error> {
        let mut cmd = pr::Command::new("sh");

        let (output_reader, output_writer) = os_pipe::pipe()?;
        let (error_reader, error_writer) = os_pipe::pipe()?;
        let (input_reader, input_writer) = os_pipe::pipe()?;

        cmd.stdout(output_writer.into_stdio());
        cmd.stderr(error_writer.into_stdio());
        cmd.stdin(input_reader.into_stdio());

        let child = cmd.spawn()?;

        drop(cmd);

        Ok(ShRemote {
            input: input_writer,
            output: output_reader,
            error: error_reader,
            child
        })
    }
}

impl Remote for ShRemote {
    fn run(&mut self, c: Command) -> Result<Response, Error> {
        let args = c.args.args.join(" "); // TODO: proper escaping!
        let tool = match c.tool {
            Tool::Path(path) => path,
        };
        let mut bytes = [0u8; 16];
        thread_rng().fill(&mut bytes);
        let key = base64::encode(&bytes);
        let key_bytes = key.as_bytes();
        let line = format!("{} {}; echo {}${{?}}e\n", tool, args, key);
        // println!("writing {:?}", line);
        self.input.write(line.as_bytes())?;
        self.input.flush()?;

        // TODO: non-blocking...

        let mut output = Vec::new();

        let mut buf = [0u8; 1024];
        let mut match_pos = 0;

        let mut loops = 10;
        'outer: loop {
            if loops <= 1 {
                break;
            }
            loops -= 1;
            let len = self.output.read(&mut buf)?;
            if len+match_pos >= key_bytes.len() {
                for i in 0 .. len+match_pos-key_bytes.len() {
                    // println!("cmp {:?}, {:?}", str::from_utf8(&buf[i .. i+key_bytes.len()-match_pos])?, str::from_utf8(&key_bytes[match_pos..])?);
                    if &buf[i .. i+key_bytes.len()-match_pos] == &key_bytes[match_pos..] {
                        output.extend(&buf[..i]);

                        // TODO: read the exit code from:
                        let _res = &buf[len-key_bytes.len()+match_pos .. len];
                        break 'outer;
                    }
                    match_pos = 0;
                }
            }
            output.extend(&buf[..len]);
        }

        Ok(Response::Text(String::from_utf8(output)?))
    }
}

trait Reader {
    fn get_command(&mut self, remote: &dyn Remote) -> Result<Command, Error>;
}

fn parse_command_simple(input: &str) -> Result<Command, Error> {
    let mut p = Parser::new();

    let cmd = p.parse(input);

    Ok(Command {
        tool: Tool::Path(cmd.head().to_string()),
        args: Args { args: cmd.body().into_iter().map(String::from).collect() },
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
    assert_eq!(c, Command {
        tool: Tool::Path(String::from("test")),
        args: Args {args: vec![
            String::from("1"),
            String::from("abc"),
            String::from("2"),
        ]}
    });
}

fn run_to_completion(s: Box<dyn Stream<Item=String, Error=Error>>)
    -> impl Future<Item=(), Error=Error>
{
    WritingFuture(s)
}

fn main() {
    let mut reader = SimpleReader;
    // let mut remote = SimpleRemote;
    let mut remote = ShRemote::new().unwrap();

    loop {
        let e = reader.get_command(&remote)
            .and_then(|cmd| remote.run(cmd))
            .and_then(|res| match res {
                Response::Text(text) => {
                    println!("{}", text);
                    Ok(())
                }
                Response::Stream { stdout } => {
                    ThreadPool::new()?
                        .run(run_to_completion(stdout))
                }
            }).err();

        if let Some(e) = e {
            println!("{:?}", e);
        }
    }
}

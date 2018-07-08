// #[macro_use]
extern crate failure;
extern crate os_pipe;
extern crate futures;
extern crate tokio;
extern crate rand;
extern crate base64;

use std::borrow::Borrow;
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

pub trait Remote {
    fn run(&mut self, c: Command) -> Result<Response, Error>;
}

struct ShRemote<I: Write, O: Read, E: Read> {
    input: I,
    output: O,
    error: E,
}

struct KeyedOutput<R: Read> {
    key: Vec<u8>,
    carryover_buf: Option<Vec<u8>>,
    match_pos: usize,
    inner: R,
}

impl<R: Read> KeyedOutput<R> {
    fn new(key: Vec<u8>, inner: R) -> KeyedOutput<R> {
        KeyedOutput {
            key,
            carryover_buf: None,
            match_pos: 0,
            inner,
        }
    }

    fn at_end(&self) -> bool {
        self.carryover_buf.is_some()
    }
}

impl<R: Read> Read for KeyedOutput<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        assert!(buf.len() >= self.key.len()); // TODO: handle this gracefully

        assert!(self.carryover_buf.is_none()); // This would mean we're at eof; todo: handle gracefully?

        let len = self.inner.read(buf)?;
        let buf = &mut buf[..len];

        if len+self.match_pos >= self.key.len() {
            for i in 0 .. len+self.match_pos-self.key.len() {
                if &buf[i .. i+self.key.len()-self.match_pos] == &self.key[self.match_pos..] {
                    self.carryover_buf = Some(buf[len-self.key.len()+self.match_pos .. len].to_vec());
                    return Ok(i);
                }
                self.match_pos = 0;
            }
        } else {
            for i in 0 .. len {
                if &buf[i..] == &self.key[i..] {
                    self.match_pos = i;
                    return Ok(0);
                }
            }
        }

        Ok(len)
    }
}

fn make_key() -> String {
    let mut bytes = [0u8; 16];
    thread_rng().fill(&mut bytes);
    base64::encode(&bytes)
}

fn spawn_sh_remote() -> Result<ShRemote<os_pipe::PipeWriter, os_pipe::PipeReader, os_pipe::PipeReader>, Error> {
    let mut cmd = pr::Command::new("sh");

    let (output_reader, output_writer) = os_pipe::pipe()?;
    let (error_reader, error_writer) = os_pipe::pipe()?;
    let (input_reader, input_writer) = os_pipe::pipe()?;

    cmd.stdout(output_writer.into_stdio());
    cmd.stderr(error_writer.into_stdio());
    cmd.stdin(input_reader.into_stdio());

    let _child = cmd.spawn()?;

    drop(cmd);

    Ok(ShRemote {
        input: input_writer,
        output: output_reader,
        error: error_reader,
    })
}

impl<I: Write, O: Read, E: Read> Remote for ShRemote<I, O, E> {
    fn run(&mut self, c: Command) -> Result<Response, Error> {
        let args = c.args.args.join(" "); // TODO: proper escaping!
        match c.tool {
            Tool::Path(path) => {
                let key = make_key();
                let line = format!("{} {}; echo {}${{?}}e\n", path, args, key);
                // println!("writing {:?}", line);
                self.input.write(line.as_bytes())?;
                self.input.flush()?;

                let mut output = Vec::new();

                let mut key_output = KeyedOutput::new(key.into_bytes(), &mut self.output);

                loop {
                    let mut buf = [0u8; 1024];
                    let len = key_output.read(&mut buf)?;
                    output.extend(&buf[..len]);
                    if key_output.at_end() {
                        break;
                    }
                }

                Ok(Response::Text(String::from_utf8(output)?))
            },
            Tool::Remote { host } => {
                let key = make_key();
                let key_err = make_key();
                let line = format!("ssh {} sh; echo {}${{?}}e; echo {} >&2\n", args, key, key_err);
                // println!("writing {:?}", line);
                self.input.write(line.as_bytes())?;
                self.input.flush()?;

                // TODO: non-blocking...

                panic!();
            }
        }
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

fn remote_run(mut remote: Box<dyn Remote>, reader: &mut dyn Reader)
    -> Result<(), Error>
{
    loop {
        let e = reader.get_command(remote.borrow())
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
                Response::Remote(remote) => {
                    remote_run(remote, reader)
                }
            }).err();

        if let Some(e) = e {
            println!("{:?}", e);
        }
    }
}

fn main() {
    let mut reader = SimpleReader;
    // let mut remote = SimpleRemote;
    let remote = Box::new(spawn_sh_remote().unwrap());

    remote_run(remote, &mut reader).unwrap();
}

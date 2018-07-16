extern crate failure;
#[macro_use]
extern crate serde_derive;
extern crate os_pipe;
extern crate futures;
extern crate tokio;
extern crate base64;
extern crate liner;
extern crate serde;
extern crate serde_json;

use os_pipe::{PipeReader, PipeWriter, IntoStdio};
use std::io::{Read, Write, BufRead, BufReader};
use std::borrow::Borrow;
use std::str;
use std::io;
use std::process as pr;
use std::thread;
use std::sync::mpsc;

use failure::Error;
use futures::{Future, Stream, Async};
use futures::task::Context;

mod parse;

use parse::Parser;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Tool {
    Path(String),
    Remote {
        host: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Args {
    args: Vec<String>
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Command {
    tool: Tool,
    args: Args,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Rpc {
    BeginCommand {
        id: usize,
        command: Command,
    },
    CommandResult {
        id: usize,
        output: String,
    }
}

// pub enum Response {
//     Text(String),
//     Stream {
//         stdout: Box<Stream<Item=String, Error=Error>>,
//     },
//     Remote(Box<dyn Remote>),
// }

pub struct Response {
    id: usize
}

pub struct BackendRemote {
    next_id: usize,
    input: PipeWriter,
    output: mpsc::Receiver<(usize, String)>,
}

fn launch_backend() -> Result<BackendRemote, Error> {
    let mut cmd = pr::Command::new("nak-backend");

    let (output_reader, output_writer) = os_pipe::pipe()?;
    let (input_reader, input_writer) = os_pipe::pipe()?;

    cmd.stdout(output_writer.into_stdio());
    cmd.stdin(input_reader.into_stdio());

    let _child = cmd.spawn()?;

    drop(cmd);

    let (sender, receiver) = mpsc::channel();
    let mut output = BufReader::new(output_reader);

    thread::spawn(move || {
        loop {
            let mut input = String::new();
            match output.read_line(&mut input) {
                Ok(_) => {
                    let rpc: Rpc = serde_json::from_str(&input).unwrap();

                    match rpc {
                        Rpc::BeginCommand { .. } => panic!(),
                        Rpc::CommandResult { id, output } => {
                            sender.send((id, output)).unwrap();
                        }
                    }
                }
                Err(error) => eprintln!("error: {}", error),
            }
        }
    });

    Ok(BackendRemote {
        next_id: 0,
        input: input_writer,
        output: receiver,
    })
}

impl BackendRemote {
    fn run(&mut self, c: Command) -> Result<Response, Error> {
        let id = self.next_id;
        self.next_id += 1;
        let serialized = serde_json::to_string(&Rpc::BeginCommand {
            id,
            command: c
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(Response {
            id
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Shell {
    DoNothing,
    Run(Command),
    Exit,
}

trait Reader {
    fn get_command(&mut self) -> Result<Shell, Error>;
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
    fn get_command(&mut self) -> Result<Shell, Error> {
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

fn one_loop(remote: &mut BackendRemote, reader: &mut dyn Reader)
    -> Result<bool, Error>
{
    match reader.get_command()? {
        Shell::Exit => return Ok(false),
        Shell::DoNothing => {}
        Shell::Run(cmd) => {
            let res = remote.run(cmd)?;

            loop {
                let (id, output) = remote.output.recv()?;

                assert_eq!(id, res.id);

                print!("{}", output);
                break;
            }
        }
    }
    Ok(true)
}

fn remote_run(mut remote: BackendRemote, reader: &mut dyn Reader)
    -> Result<(), Error>
{
    while one_loop(&mut remote, reader)? {}
    Ok(())
}

fn main() {
    let mut reader = SimpleReader::new();
    // let remote = Box::new(SimpleRemote);
    let remote = launch_backend().unwrap();

    remote_run(remote, &mut reader).unwrap();
}

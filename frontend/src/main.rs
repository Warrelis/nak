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

use os_pipe::{PipeWriter, IntoStdio};
use std::io::{Write, BufRead, BufReader};
use std::str;
use std::io;
use std::process as pr;
use std::thread;
use std::sync::mpsc;

use failure::Error;

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
pub enum RpcRequest {
    BeginCommand {
        id: usize,
        stdout_pipe: usize,
        stderr_pipe: usize,
        command: Command,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcResponse {
    Pipe {
        id: usize,
        data: Vec<u8>,
    },
    CommandDone {
        id: usize,
        exit_code: i64,
    },
}

pub struct Response {
    id: usize,
    stdout_pipe: usize,
    stderr_pipe: usize,
}

pub struct BackendRemote {
    next_id: usize,
    input: PipeWriter,
    output: mpsc::Receiver<RpcResponse>,
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
                    let rpc: RpcResponse = serde_json::from_str(&input).unwrap();

                    sender.send(rpc).unwrap();
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

    fn next_id(&mut self) -> usize {
        let res = self.next_id;
        self.next_id += 1;
        res
    }

    fn run(&mut self, c: Command) -> Result<Response, Error> {
        let id = self.next_id();
        let stdout_pipe = self.next_id();
        let stderr_pipe = self.next_id();
        let serialized = serde_json::to_string(&RpcRequest::BeginCommand {
            id,
            stdout_pipe,
            stderr_pipe,
            command: c
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(Response {
            id,
            stdout_pipe,
            stderr_pipe,
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
                let msg = remote.output.recv()?;

                match msg {
                    RpcResponse::Pipe { id, data } => {
                        if id == res.stdout_pipe {
                            io::stdout().write(&data)?;
                        } else if id == res.stderr_pipe {
                            io::stderr().write(&data)?;
                        } else {
                            assert!(false);
                        }
                    }
                    RpcResponse::CommandDone { id, exit_code } => {
                        assert_eq!(id, res.id);
                        println!("exit_code: {}", exit_code);
                        break;
                    }
                }
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

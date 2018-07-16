#[macro_use]
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
pub enum Command {
    Unknown(String, Args),
    SetDirectory(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Args {
    args: Vec<String>
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Multiplex<M> {
    remote_id: usize,
    message: M
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcRequest {
    BeginCommand {
        id: usize,
        stdout_pipe: usize,
        stderr_pipe: usize,
        command: Command,
    },
    BeginRemote {
        id: usize,
        command: Command,
    }
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

pub struct Remote {
    id: usize,
}

pub struct BackendRemote {
    next_id: usize,
    remotes: Vec<Remote>,
    input: PipeWriter,
    output: mpsc::Receiver<Multiplex<RpcResponse>>,
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

    let mut input = String::new();
    output.read_line(&mut input)?;
    assert_eq!(input, "nxQh6wsIiiFomXWE+7HQhQ==\n");

    thread::spawn(move || {
        loop {
            let mut input = String::new();
            match output.read_line(&mut input) {
                Ok(_) => {
                    let rpc: Multiplex<RpcResponse> = serde_json::from_str(&input).unwrap();

                    sender.send(rpc).unwrap();
                }
                Err(error) => eprintln!("error: {}", error),
            }
        }
    });

    Ok(BackendRemote {
        next_id: 0,
        remotes: Vec::new(),
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

    fn push_remote(&mut self, remote: Remote) {
        self.remotes.push(remote);
    }

    fn run(&mut self, c: Command) -> Result<Response, Error> {
        let id = self.next_id();
        let stdout_pipe = self.next_id();
        let stderr_pipe = self.next_id();
        let serialized = serde_json::to_string(&Multiplex {
            remote_id: self.remotes.len(),
            message: RpcRequest::BeginCommand {
                id,
                stdout_pipe,
                stderr_pipe,
                command: c
            },
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(Response {
            id,
            stdout_pipe,
            stderr_pipe,
        })
    }

    fn begin_remote(&mut self, c: Command) -> Result<Remote, Error> {
        let id = self.next_id();

        let serialized = serde_json::to_string(&Multiplex {
            remote_id: 0,
            message: RpcRequest::BeginRemote {
                id,
                command: c
            },
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(Remote {
            id,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Shell {
    DoNothing,
    Run(Command),
    BeginRemote(Command),
    Exit,
}

trait Reader {
    fn get_command(&mut self, backend: &BackendRemote) -> Result<Shell, Error>;
}

fn check_single_arg(items: &[&str]) -> Result<String, Error> {
    if items.len() == 1 {
        Ok(items[0].to_string())
    } else {
        Err(format_err!("Bad argument length: {}", items.len()))
    }
}

fn parse_command_simple(input: &str) -> Result<Shell, Error> {
    let mut p = Parser::new();

    if input.len() == 0 {
        return Ok(Shell::DoNothing);
    }

    let cmd = p.parse(input);

    let head = cmd.head();

    Ok(Shell::Run(match head {
        "cd" => Command::SetDirectory(check_single_arg(&cmd.body())?),
        "nak" => {
            let mut it = cmd.body().into_iter();
            let head = it.next().unwrap().to_string();

            return Ok(Shell::BeginRemote(Command::Unknown(
                head,
                Args { args: it.map(String::from).collect() },
            )));
        }
        _ => Command::Unknown(
            head.to_string(),
            Args { args: cmd.body().into_iter().map(String::from).collect() },
        )
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
    fn get_command(&mut self, backend: &BackendRemote) -> Result<Shell, Error> {
        let res = match self.ctx.read_line(format!("[{}]$ ", backend.remotes.len()), &mut |_| {}) {
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
    assert_eq!(c, Shell::Run(Command::Unknown(
        String::from("test"),
        Args {args: vec![
            String::from("1"),
            String::from("abc"),
            String::from("2"),
        ]}
    )));
}

fn one_loop(remote: &mut BackendRemote, reader: &mut dyn Reader)
    -> Result<bool, Error>
{
    match reader.get_command(remote)? {
        Shell::Exit => {
            if remote.remotes.pop().is_none() {
                return Ok(false)
            }
        }
        Shell::DoNothing => {}
        Shell::BeginRemote(cmd) => {
            let res = remote.begin_remote(cmd)?;
            remote.push_remote(res);
        }
        Shell::Run(cmd) => {
            let res = remote.run(cmd)?;

            loop {
                let msg = remote.output.recv()?;

                assert_eq!(msg.remote_id, remote.remotes.len());
                match msg.message {
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

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
extern crate ctrlc;
extern crate libc;

use os_pipe::{PipeWriter, IntoStdio};
use std::io::{Write, BufRead, BufReader};
use std::str;
use std::{io, fs};
use std::process as pr;
use std::thread;
use std::sync::mpsc;

use failure::Error;

mod parse;
mod edit;
mod prefs;

use parse::Parser;
use prefs::Prefs;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Unknown(String, Args),
    SetDirectory(String),
}

impl Command {
    pub fn add_args(&mut self, new_args: Vec<String>) {
        match self {
            &mut Command::Unknown(_, ref mut args) => {
                args.args.extend(new_args)
            }
            &mut Command::SetDirectory(_) => panic!(), 
        }
    }
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
    CancelCommand {
        id: usize,
    },
    BeginRemote {
        id: usize,
        command: Command,
    },
    EndRemote {
        id: usize,
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

pub struct Pipes {
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
}

enum Event {
    Remote(Multiplex<RpcResponse>),
    CtrlC,
}

fn launch_backend(sender: mpsc::Sender<Event>) -> Result<BackendRemote, Error> {

    let (output_reader, output_writer) = os_pipe::pipe()?;
    let (input_reader, input_writer) = os_pipe::pipe()?;

    let pid = unsafe { libc::fork() };

    if pid == 0 {
        let mut cmd = pr::Command::new("nak-backend");

        cmd.stdout(output_writer.into_stdio());
        cmd.stderr(fs::OpenOptions::new().create(true).append(true).open("/tmp/nak.log")?);
        cmd.stdin(input_reader.into_stdio());

        let _child = cmd.spawn()?;

        drop(cmd);
        unsafe { libc::exit(0) };
    }

    let mut output = BufReader::new(output_reader);

    let mut input = String::new();
    output.read_line(&mut input)?;
    assert_eq!(input, "nxQh6wsIiiFomXWE+7HQhQ==\n");

    thread::spawn(move || {
        loop {
            let mut input = String::new();
            match output.read_line(&mut input) {
                Ok(n) => {
                    if n == 0 {
                        continue;
                    }
                    let rpc: Multiplex<RpcResponse> = serde_json::from_str(&input).unwrap();

                    sender.send(Event::Remote(rpc)).unwrap();
                }
                Err(error) => eprintln!("error: {}", error),
            }
        }
    });

    Ok(BackendRemote {
        next_id: 0,
        remotes: Vec::new(),
        input: input_writer,
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

    fn run(&mut self, c: Command) -> Result<Pipes, Error> {
        let id = self.next_id();
        let stdout_pipe = self.next_id();
        let stderr_pipe = self.next_id();
        let serialized = serde_json::to_string(&Multiplex {
            remote_id: self.remotes.last().map(|r| r.id).unwrap_or(0),
            message: RpcRequest::BeginCommand {
                id,
                stdout_pipe,
                stderr_pipe,
                command: c
            },
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(Pipes {
            id,
            stdout_pipe,
            stderr_pipe,
        })
    }

    fn begin_remote(&mut self, c: Command) -> Result<Remote, Error> {
        let id = self.next_id();

        let serialized = serde_json::to_string(&Multiplex {
            remote_id: self.remotes.last().map(|r| r.id).unwrap_or(0),
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

    fn end_remote(&mut self, remote: Remote) -> Result<(), Error> {
        let serialized = serde_json::to_string(&Multiplex {
            remote_id: self.remotes.last().map(|r| r.id).unwrap_or(0),
            message: RpcRequest::EndRemote {
                id: remote.id,
            },
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(())
    }

    fn cancel(&mut self, id: usize) -> Result<(), Error> {

        let serialized = serde_json::to_string(&Multiplex {
            remote_id: self.remotes.last().map(|r| r.id).unwrap_or(0),
            message: RpcRequest::CancelCommand { id },
        })?;

        eprintln!("caught CtrlC");
        write!(self.input, "{}\n", serialized)?;
        self.input.flush()?;

        Ok(())
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

fn parse_command_simple(prefs: &Prefs, input: &str) -> Result<Shell, Error> {
    let mut p = Parser::new();

    if input.len() == 0 {
        return Ok(Shell::DoNothing);
    }

    let cmd = p.parse(input);

    let head = cmd.head();

    if let Some(mut new_cmd) = prefs.expand(head) {
        new_cmd.add_args(cmd.body().into_iter().map(String::from).collect());
        return Ok(Shell::Run(new_cmd));
    }

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

struct SimpleCompleter;

impl liner::Completer for SimpleCompleter {
    fn completions(&self, _start: &str) -> Vec<String> {
        vec![]
    }
}

struct SimpleReader {
    prefs: Prefs,
    ctx: liner::Context,
}

impl SimpleReader {
    fn new(prefs: Prefs) -> SimpleReader {
        SimpleReader {
            prefs,
            ctx: liner::Context {
                history: liner::History::new(),
                completer: Some(Box::new(SimpleCompleter)),
                word_divider_fn: Box::new(liner::get_buffer_words),
                key_bindings: liner::KeyBindings::Emacs,
            }
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

        Ok(parse_command_simple(&self.prefs, &res)?)
    }
}

#[test]
fn parse_simple() {
    let c = parse_command_simple(&Prefs::default(), " test 1 abc 2").unwrap();
    assert_eq!(c, Shell::Run(Command::Unknown(
        String::from("test"),
        Args {args: vec![
            String::from("1"),
            String::from("abc"),
            String::from("2"),
        ]}
    )));
}

enum TermState {
    ReadingCommand,
    WaitingOn(Pipes),
}

struct Exec {
    receiver: mpsc::Receiver<Event>,
    remote: BackendRemote,
    reader: Box<dyn Reader>,
    state: TermState,
}

impl Exec {
    fn one_loop(&mut self) -> Result<bool, Error> {
        match self.state {
            TermState::ReadingCommand => {
                let cmd = self.reader.get_command(&mut self.remote)?;

                match cmd {
                    Shell::Exit => {
                        if let Some(remote) = self.remote.remotes.pop() {
                            self.remote.end_remote(remote)?;
                        } else {
                            return Ok(false)
                        }
                    }
                    Shell::DoNothing => {}
                    Shell::BeginRemote(cmd) => {
                        let res = self.remote.begin_remote(cmd)?;
                        self.remote.push_remote(res);
                    }
                    Shell::Run(cmd) => {
                        let res = self.remote.run(cmd)?;
                        self.state = TermState::WaitingOn(res);
                    }
                }
            }
            TermState::WaitingOn(Pipes { id, stdout_pipe, stderr_pipe }) => {
                let msg = self.receiver.recv()?;

                match msg {
                    Event::Remote(msg) => {
                        assert_eq!(msg.remote_id, self.remote.remotes.last().map(|x| x.id).unwrap_or(0));

                        match msg.message {
                            RpcResponse::Pipe { id, data } => {
                                if id == stdout_pipe {
                                    io::stdout().write(&data)?;
                                } else if id == stderr_pipe {
                                    io::stderr().write(&data)?;
                                } else {
                                    assert!(false, "{} {} {}", id, stdout_pipe, stderr_pipe);
                                }
                            }
                            RpcResponse::CommandDone { id, exit_code: _ } => {
                                assert_eq!(id, id);
                                // println!("exit_code: {}", exit_code);
                                self.state = TermState::ReadingCommand;
                            }
                        }
                    }
                    Event::CtrlC => {
                        self.remote.cancel(id)?;
                    }
                }
            }
        }
        Ok(true)
    }
}

fn remote_run(receiver: mpsc::Receiver<Event>, remote: BackendRemote, reader: Box<dyn Reader>)
    -> Result<(), Error>
{
    let mut exec = Exec {
        receiver,
        remote,
        reader,
        state: TermState::ReadingCommand,
    };

    while exec.one_loop()? {}
    Ok(())
}

fn main() -> Result<(), Error> {
    let prefs = Prefs::load()?;
    // let remote = Box::new(SimpleRemote);
    let (sender, receiver) = mpsc::channel();

    let sender_clone = sender.clone();

    ctrlc::set_handler(move || {
        sender_clone.send(Event::CtrlC).unwrap();
    }).expect("Error setting CtrlC handler");

    let remote = launch_backend(sender)?;

    remote_run(receiver, remote, Box::new(SimpleReader::new(prefs)))?;
    Ok(())
}

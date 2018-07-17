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
extern crate termion;

use std::io::Write;
use std::str;
use std::io;
use std::sync::mpsc;

use failure::Error;

mod parse;
mod edit;
mod prefs;
mod comm;

use prefs::Prefs;
use comm::{BackendRemote, launch_backend};
use edit::{Reader, SimpleReader};

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

pub enum Event {
    Remote(Multiplex<RpcResponse>),
    CtrlC,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shell {
    DoNothing,
    Run(Command),
    BeginRemote(Command),
    Exit,
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

    exec.reader.save_history();

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

    remote_run(receiver, remote, Box::new(SimpleReader::new(prefs)?))?;

    Ok(())
}

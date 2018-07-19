
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
extern crate tempfile;
extern crate protocol;

use std::io::Read;
use std::io::Write;
use std::io;
use std::sync::mpsc;
use std::process as pr;
use std::fs::File;

use failure::Error;
use tempfile::tempdir;

use protocol::{Multiplex, RpcResponse, Command, Process};

mod parse;
mod edit;
mod prefs;
mod comm;

use prefs::Prefs;
use comm::{BackendRemote, launch_backend};
use edit::{Reader, SimpleReader};

#[derive(Debug)]
pub enum Event {
    Remote(Multiplex<RpcResponse>),
    Key(termion::event::Key),
    CtrlC,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shell {
    DoNothing,
    Run {
        cmd: Command,
        redirect: Option<String>,
    },
    BeginRemote(Command),
    Exit,
}

enum TermState {
    ReadingCommand,
    WaitingOn(Process),
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
                        if self.remote.remotes.len() > 1 {
                            self.remote.end_remote()?;
                        } else {
                            return Ok(false)
                        }
                    }
                    Shell::DoNothing => {}
                    Shell::BeginRemote(cmd) => {
                        self.remote.begin_remote(cmd)?;
                    }
                    Shell::Run { cmd, redirect } => {
                        let res = self.remote.run(cmd, redirect)?;
                        self.state = TermState::WaitingOn(res);
                    }
                }
            }
            TermState::WaitingOn(Process { id, stdout_pipe, stderr_pipe }) => {
                let msg = self.receiver.recv()?;

                match msg {
                    Event::Remote(msg) => {
                        // assert_eq!(msg.remote_id, self.remote.remotes.last().map(|x| x.id).unwrap_or(0));

                        match msg.message {
                            RpcResponse::Pipe { id, data } => {
                                if id == stdout_pipe {
                                    io::stdout().write(&data)?;
                                    // io::stdout().flush()?;
                                } else if id == stderr_pipe {
                                    io::stderr().write(&data)?;
                                    // io::stdout().flush()?;
                                } else {
                                    assert!(false, "{} {} {}", id, stdout_pipe, stderr_pipe);
                                }
                            }
                            RpcResponse::CommandDone { id, exit_code: _ } => {
                                assert_eq!(id, id);
                                // println!("exit_code: {}", exit_code);
                                self.state = TermState::ReadingCommand;
                            }
                            RpcResponse::DirectoryListing { .. } => {
                                panic!();
                            }
                            RpcResponse::EditRequest { edit_id, command_id, name, data } => {
                                println!("editing {}", name);
                                io::stdout().write(&data).unwrap();

                                let name = &name[name.rfind("/").map(|x| x+1).unwrap_or(0)..];

                                let dir = tempdir()?;
                                let path = dir.path().join(name);
                                File::create(&path)?.write_all(&data)?;

                                let res = pr::Command::new("micro").arg(path.to_str().unwrap()).status()?;
                                assert!(res.success());

                                let mut new_data = Vec::new();
                                File::open(path)?.read_to_end(&mut new_data)?;

                                self.remote.finish_edit(command_id, edit_id, new_data).unwrap();
                            }
                        }
                    }
                    Event::CtrlC => {
                        self.remote.cancel(id)?;
                    }
                    Event::Key(_) => {
                        panic!();
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

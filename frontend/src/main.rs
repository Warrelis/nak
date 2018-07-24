
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

use std::sync::mpsc;

use failure::Error;

use protocol::{Response, Command, ReadProcess};

mod parse;
mod edit;
mod prefs;
mod comm;
mod plan;

use prefs::Prefs;
use comm::{BackendEndpoint, launch_backend, EndpointExt};
use edit::{SimpleReader};
use plan::{Plan, RemoteStep, Step};

#[derive(Debug)]
pub enum Event {
    Remote(Response),
    Key(termion::event::Key),
    CtrlC,
}

#[derive(Debug, PartialEq)]
pub enum Shell {
    DoNothing,
    Run {
        cmd: Command,
        redirect: Option<String>,
    },
    Plan(Plan),
    BeginRemote(Command),
    Exit,
}

struct Exec {
    receiver: mpsc::Receiver<Event>,
    remote: BackendEndpoint,
    reader: SimpleReader,
}

impl Exec {
    fn one_loop(&mut self) -> Result<bool, Error> {
        if let Some(_) = self.remote.handler.waiting_for_remote {
            let msg = self.receiver.recv()?;

            match msg {
                Event::Remote(msg) => {
                    self.remote.receive(msg.clone())?;
                }
                Event::CtrlC => {
                    panic!();
                }
                Event::Key(_) => {
                    panic!();
                }
            }
        } else {
            match self.remote.handler.waiting_for {
                None => {
                    let prompt = {
                        let top_remote = &self.remote.handler.remotes.last().unwrap().1;
                        format!("[{}:{}] {}$ ",
                            self.remote.handler.remotes.len(),
                            top_remote.hostname,
                            top_remote.working_dir)
                    };
                    let plan = self.reader.get_command(prompt, &mut self.remote)?;

                    for step in plan.steps {
                        match step {
                            Step::Remote(_remote_id, RemoteStep::Run(cmd, _pr)) => {
                                let res = self.remote.run(cmd, None)?;
                                self.remote.handler.waiting_for = Some(res);
                            }
                            Step::Remote(_remote_id, RemoteStep::Close) => {
                                if self.remote.handler.remotes.len() > 1 {
                                    self.remote.end_remote()?;
                                } else {
                                    return Ok(false)
                                }
                            }
                            _ => panic!()
                        }
                    }

                    // match cmd {
                    //     Shell::Exit => {
                    //         if self.remote.handler.remotes.len() > 1 {
                    //             self.remote.end_remote()?;
                    //         } else {
                    //             return Ok(false)
                    //         }
                    //     }
                    //     Shell::DoNothing => {}
                    //     Shell::BeginRemote(cmd) => {
                    //         self.remote.begin_remote(cmd)?;
                    //     }
                    //     Shell::Run { cmd, redirect } => {
                    //         let res = self.remote.run(cmd, redirect)?;
                    //         self.remote.handler.waiting_for = Some(res);
                    //     }
                    //     Shell::Plan(plan) => {
                    //         // let res = self.remote.run(cmd, redirect)?;
                    //         // self.remote.handler.waiting_for = Some(res);
                    //         panic!();
                    //     }
                    // }

                }
                Some(ReadProcess { id, .. }) => {
                    let msg = self.receiver.recv()?;

                    match msg {
                        Event::Remote(msg) => {
                            self.remote.receive(msg.clone())?;
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
        }
        Ok(true)
    }
}

fn remote_run(receiver: mpsc::Receiver<Event>, remote: BackendEndpoint, reader: SimpleReader)
    -> Result<(), Error>
{
    let mut exec = Exec {
        receiver,
        remote,
        reader,
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

    remote_run(receiver, remote, SimpleReader::new(prefs)?)?;

    Ok(())
}

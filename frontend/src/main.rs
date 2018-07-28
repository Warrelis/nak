
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
use std::collections::{HashMap, HashSet};

use failure::Error;

use protocol::{Response, Command, WritePipes, ProcessId, Condition};

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
            if self.remote.handler.waiting_for.len() == 0 {
                for (remote, stream_id) in self.remote.handler.cwd_for_remote.drain() {

                    let mut result = self.remote.handler.gathering_output.remove(&stream_id).unwrap();

                    for (id, rem) in self.remote.handler.remotes.iter_mut() {
                        if *id == remote {
                            assert_eq!(result.pop(), Some(b'\n'));
                            let text = String::from_utf8(result).unwrap();
                            rem.working_dir = text;
                            break;
                        }
                    }
                }
                let prompt = {
                    let top_remote = &self.remote.handler.remotes.last().unwrap().1;
                    format!("[{}:{}] {}$ ",
                        self.remote.handler.remotes.len(),
                        top_remote.hostname,
                        top_remote.working_dir)
                };

                let plan = self.reader.get_command(prompt, &mut self.remote)?;

                let mut pipe_pairs = Vec::new();

                let mut wait = HashSet::new();

                for step in plan.steps {
                    match step {
                        Step::Pipe => {
                            let (r, w) = self.remote.pipe();
                            pipe_pairs.push((Some(r), Some(w)));
                        }
                        Step::Remote(_remote_id, RemoteStep::Run(cmd, pr)) => {
                            let remote = self.remote.cur_remote();
                            let stdout = pipe_pairs[pr.stdout].1.take().unwrap();
                            let stderr = pipe_pairs[pr.stderr].1.take().unwrap();
                            let stdin = pipe_pairs[pr.stdin].0.take().unwrap();
                            let pid = self.remote.command(remote, cmd, HashMap::new(), WritePipes {
                                stdin, stdout, stderr
                            })?;
                            wait.insert(pid);
                        }
                        Step::Remote(_remote_id, RemoteStep::Close) => {
                            if self.remote.handler.remotes.len() > 1 {
                                self.remote.end_remote()?;
                            } else {
                                return Ok(false)
                            }
                        }
                        Step::Remote(_remote_id, RemoteStep::OpenOutputFile(path)) => {
                            let remote = self.remote.cur_remote();

                            let handle = self.remote.open_output_file(remote, path)?;
                            pipe_pairs.push((None, Some(handle)));
                            // let handle = self.remote.
                        }
                        _ => panic!()
                    }
                }

                {
                    let remote = self.remote.cur_remote();
                    let (stdout_read, stdout_write) = self.remote.pipe();
                    let (_stderr_read, stderr_write) = self.remote.pipe();
                    let (stdin_read, _stdin_write) = self.remote.pipe();
                    let mut block_on: HashMap<ProcessId, Condition> = wait.iter().map(|&pid| (pid, None)).collect();

                    let pid = self.remote.command(remote, Command::GetDirectory, block_on, WritePipes {
                        stdin: stdin_read,
                        stdout: stdout_write,
                        stderr: stderr_write,
                    })?;

                    self.remote.handler.gathering_output.insert(stdout_read, Vec::new());

                    self.remote.handler.cwd_for_remote.insert(remote, stdout_read);

                    wait.insert(pid);
                }

                assert!(self.remote.handler.waiting_for.len() == 0);
                self.remote.handler.waiting_for = wait;

            } else {
                let msg = self.receiver.recv()?;

                match msg {
                    Event::Remote(msg) => {
                        self.remote.receive(msg.clone())?;
                    }
                    Event::CtrlC => {
                        let to_cancel = self.remote.handler.waiting_for.drain().collect::<Vec<ProcessId>>();
                        for id in to_cancel {
                            self.remote.cancel(id)?;
                        }
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

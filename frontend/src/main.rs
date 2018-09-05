#![allow(unused)]

#[macro_use]
extern crate failure;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate structopt_derive;
extern crate structopt;
extern crate os_pipe;
extern crate base64;
extern crate liner;
extern crate serde;
extern crate serde_json;
extern crate ctrlc;
extern crate libc;
extern crate termion;
extern crate tempfile;
extern crate protocol;
extern crate dirs;

use std::sync::mpsc;
use std::collections::{HashMap, HashSet};

use failure::Error;
use structopt::StructOpt;

use protocol::{Response, Command, WritePipes, ProcessId, Condition};

mod parse;
mod edit;
mod prefs;
mod comm;
mod plan;

use crate::prefs::Prefs;
use crate::comm::{BackendEndpoint, launch_backend, EndpointExt};
use crate::edit::{SimpleReader, Reader, SingleCommandReader};
use crate::plan::{Plan, RemoteStep, Step};

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

struct Exec<R: Reader> {
    receiver: mpsc::Receiver<Event>,
    remote: BackendEndpoint,
    reader: R,
}

impl<R: Reader> Exec<R> {
    fn one_loop(&mut self) -> Result<bool, Error> {
        if self.remote.handler.waiting_for_remote.is_some() {
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
            // eprintln!("waiting for {:?} {:?}", self.remote.handler.waiting_for, self.remote.handler.waiting_for_eof);
            if self.remote.handler.waiting_for.len() == 0 && self.remote.handler.waiting_for_eof.len() == 0 {
                for (remote, stream_id) in self.remote.handler.cwd_for_remote.drain() {

                    let mut result = self.remote.handler.finished_output.remove(&stream_id).unwrap();

                    for (id, rem) in self.remote.handler.remotes.iter_mut() {
                        if *id == remote {
                            if result.pop() == Some(b'\n') {
                                let text = String::from_utf8(result).unwrap();
                                rem.working_dir = text;
                                break;
                            }
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
                    let (stderr_read, stderr_write) = self.remote.pipe();
                    let (stdin_read, _stdin_write) = self.remote.pipe();
                    let block_on: HashMap<ProcessId, Condition> = wait.iter().map(|&pid| (pid, None)).collect();

                    let pid = self.remote.command(remote, Command::GetDirectory, block_on, WritePipes {
                        stdin: stdin_read,
                        stdout: stdout_write,
                        stderr: stderr_write,
                    })?;

                    self.remote.pipe_begin_read(remote, stdout_read)?;
                    self.remote.pipe_begin_read(remote, stderr_read)?;
                    self.remote.pipe_read(remote, stdout_read, 1024*1024)?;
                    self.remote.pipe_read(remote, stderr_read, 1024*1024)?;

                    self.remote.handler.gathering_output.insert(stdout_read.to_generic(), Vec::new());
                    self.remote.handler.waiting_for_eof.insert(stdout_read.to_generic());

                    self.remote.handler.stderr_pipes.insert(stderr_read.to_generic());

                    self.remote.handler.cwd_for_remote.insert(remote, stdout_read.to_generic());

                    wait.insert(pid);
                }

                let remote = self.remote.cur_remote();

                for pipe in plan.stdout {
                    let stdout = pipe_pairs[pipe].0.take().unwrap();
                    self.remote.pipe_begin_read(remote, stdout)?;
                    self.remote.pipe_read(remote, stdout, 1024*1024)?;
                    self.remote.handler.stdout_pipes.insert(stdout.to_generic());
                }

                for pipe in plan.stderr {
                    let stderr = pipe_pairs[pipe].0.take().unwrap();
                    self.remote.pipe_begin_read(remote, stderr)?;
                    self.remote.pipe_read(remote, stderr, 1024*1024)?;
                    self.remote.handler.stderr_pipes.insert(stderr.to_generic());
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
                        for id in self.remote.handler.waiting_for.iter().cloned().collect::<Vec<_>>() {
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

fn remote_run(receiver: mpsc::Receiver<Event>, remote: BackendEndpoint, reader: impl Reader)
    -> Result<(), Error>
{
    let mut exec = Exec {
        receiver,
        remote,
        reader,
    };

    while exec.one_loop()? {}

    exec.reader.hacky_save_history();

    Ok(())
}

#[derive(StructOpt, Debug)]
#[structopt(name = "nak")]
struct MainOptions {
    #[structopt(long = "backend")]
    backend: Option<String>,

    #[structopt(long = "command")]
    command: Option<String>,
}

fn main() -> Result<(), Error> {
    let prefs = Prefs::load()?;
    // let remote = Box::new(SimpleRemote);
    let (sender, receiver) = mpsc::channel();

    let sender_clone = sender.clone();

    let args = MainOptions::from_args();

    let remote = launch_backend(sender, args.backend)?;

    if let Some(command) = args.command {
        remote_run(receiver, remote, SingleCommandReader::new(prefs, command))?;
    } else {
        ctrlc::set_handler(move || {
            sender_clone.send(Event::CtrlC).unwrap();
        }).expect("Error setting CtrlC handler");

        remote_run(receiver, remote, SimpleReader::new(prefs)?)?;
    }

    Ok(())
}

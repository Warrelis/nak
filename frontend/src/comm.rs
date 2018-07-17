
use std::sync::mpsc;
use std::process as pr;
use std::thread;
use std::fs;
use std::io::{BufRead, BufReader, Write};

use failure::Error;
use os_pipe::{PipeWriter, IntoStdio};
use os_pipe;
use libc;
use serde_json;

use { RpcResponse, RpcRequest, Pipes, Remote, Event, Multiplex, Command };

pub struct BackendRemote {
    next_id: usize,
    pub remotes: Vec<Remote>,
    input: PipeWriter,
}

pub fn launch_backend(sender: mpsc::Sender<Event>) -> Result<BackendRemote, Error> {

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

    pub fn push_remote(&mut self, remote: Remote) {
        self.remotes.push(remote);
    }

    pub fn run(&mut self, c: Command) -> Result<Pipes, Error> {
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

    pub fn begin_remote(&mut self, c: Command) -> Result<Remote, Error> {
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

    pub fn end_remote(&mut self, remote: Remote) -> Result<(), Error> {
        let serialized = serde_json::to_string(&Multiplex {
            remote_id: self.remotes.last().map(|r| r.id).unwrap_or(0),
            message: RpcRequest::EndRemote {
                id: remote.id,
            },
        })?;

        write!(self.input, "{}\n", serialized)?;

        Ok(())
    }

    pub fn cancel(&mut self, id: usize) -> Result<(), Error> {

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
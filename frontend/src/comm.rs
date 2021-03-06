use std::sync::mpsc;
use std::process as pr;
use std::thread;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write, Read};
use std::io;
use std::collections::{HashMap, HashSet};

use failure::Error;
use os_pipe::{PipeWriter, IntoStdio};
use os_pipe;
use libc;
use serde_json;
use tempfile::tempdir;

use protocol::{
    Response,
    Command,
    Transport,
    Endpoint,
    RemoteId,
    EndpointHandler,
    ProcessId,
    WritePipe,
    ReadPipe,
    ReadPipes,
    ReadProcess,
    RemoteInfo,
    WritePipes,
    PipeMessage,
    GenericPipe,
};

use crate::Event;

pub struct PipeTransport {
    input: PipeWriter,
}

impl Transport for PipeTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        self.input.write(data)?;
        self.input.flush()?;
        Ok(())
    }
}

pub struct StackedRemotes {
    pub remotes: Vec<(RemoteId, RemoteInfo)>,
    pub waiting_for: HashSet<ProcessId>,
    pub waiting_for_eof: HashSet<GenericPipe>,
    pub waiting_for_remote: Option<RemoteId>,
    pub gathering_output: HashMap<GenericPipe, Vec<u8>>,
    pub finished_output: HashMap<GenericPipe, Vec<u8>>,
    pub cwd_for_remote: HashMap<RemoteId, GenericPipe>,
    pub stdout_pipes: HashSet<GenericPipe>,
    pub stderr_pipes: HashSet<GenericPipe>,
}

impl<T: Transport> EndpointHandler<T> for StackedRemotes {
    fn remote_ready(endpoint: &mut Endpoint<T, Self>, id: RemoteId, remote_info: RemoteInfo) -> Result<(), Error> {
        assert_eq!(endpoint.handler.waiting_for_remote.expect("no remote waiting"), id);
        endpoint.handler.waiting_for_remote = None;
        endpoint.handler.remotes.push((id, remote_info));
        Ok(())
    }

    fn pipe(endpoint: &mut Endpoint<T, Self>, id: GenericPipe, msg: PipeMessage) -> Result<(), Error> {
        match msg {
            PipeMessage::Data { data, end_offset: _ } => {
                if let Some(out) = endpoint.handler.gathering_output.get_mut(&id) {
                    out.extend(data)
                } else if endpoint.handler.stdout_pipes.contains(&id) {
                    io::stdout().write_all(&data)?;
                } else if endpoint.handler.stderr_pipes.contains(&id) {
                    io::stderr().write_all(&data)?;
                } else {
                    panic!("bad pipe {:?} {:?} {:?}", id, endpoint.handler.stdout_pipes, endpoint.handler.stderr_pipes);
                }
            }
            PipeMessage::Closed { .. } => {
                endpoint.handler.waiting_for_eof.remove(&id);
                if let Some(output) = endpoint.handler.gathering_output.remove(&id) {
                    endpoint.handler.finished_output.insert(id, output);
                }
            }
            _ => panic!("{:?}", msg),
        }
        Ok(())
    }

    fn command_done(endpoint: &mut Endpoint<T, Self>, id: ProcessId, _exit_code: i64) -> Result<(), Error> {
        assert!(endpoint.handler.waiting_for.remove(&id));
        Ok(())
    }

    fn directory_listing(_endpoint: &mut Endpoint<T, Self>, _id: usize, _items: Vec<String>) -> Result<(), Error> {
        panic!();
    }

    fn edit_request(endpoint: &mut Endpoint<T, Self>, edit_id: usize, command_id: ProcessId, name: String, data: Vec<u8>) -> Result<(), Error> {
        println!("editing {}", name);
        io::stdout().write(&data)?;

        let name = &name[name.rfind("/").map(|x| x+1).unwrap_or(0)..];

        let dir = tempdir()?;
        let path = dir.path().join(name);
        File::create(&path)?.write_all(&data)?;

        let res = pr::Command::new("micro").arg(path.to_str().unwrap()).status()?;
        assert!(res.success());

        let mut new_data = Vec::new();
        File::open(path)?.read_to_end(&mut new_data)?;

        endpoint.finish_edit(command_id, edit_id, new_data)?;
        Ok(())
    }
}

pub type BackendEndpoint = Endpoint<PipeTransport, StackedRemotes>;

pub fn launch_backend(sender: mpsc::Sender<Event>, name: Option<String>) -> Result<BackendEndpoint, Error> {

    let (output_reader, output_writer) = os_pipe::pipe()?;
    let (input_reader, input_writer) = os_pipe::pipe()?;

    let pid = unsafe { libc::fork() };

    if pid == 0 {
        let mut cmd = pr::Command::new(name.unwrap_or(String::from("nak-backend")));

        cmd.env("RUST_BACKTRACE", "1");
        cmd.stdout(output_writer.into_stdio());
        cmd.stderr(fs::OpenOptions::new().create(true).append(true).open("/tmp/nak.log")?);
        cmd.stdin(input_reader.into_stdio());

        let _child = cmd.spawn()?;

        drop(cmd);
        unsafe { libc::exit(0) };
    }

    let stacked_remotes = StackedRemotes {
        remotes: vec![],
        waiting_for: HashSet::new(),
        waiting_for_eof: HashSet::new(),
        waiting_for_remote: None,
        gathering_output: HashMap::new(),
        finished_output: HashMap::new(),
        cwd_for_remote: HashMap::new(),
        stdout_pipes: HashSet::new(),
        stderr_pipes: HashSet::new(),
    };

    let mut endpoint = Endpoint::new(
        PipeTransport { input: input_writer },
        stacked_remotes);

    let root = endpoint.root();
    endpoint.handler.waiting_for_remote = Some(root);

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
                    let rpc: Response = serde_json::from_str(&input).unwrap();

                    sender.send(Event::Remote(rpc)).unwrap();
                }
                Err(error) => eprintln!("error: {}", error),
            }
        }
    });

    Ok(endpoint)
}

pub trait EndpointExt {
    fn cur_remote(&self) -> RemoteId;

    fn run(&mut self, c: Command, redirect: Option<String>) -> Result<ReadProcess, Error>;

    fn begin_remote(&mut self, c: Command) -> Result<RemoteId, Error>;

    fn end_remote(&mut self) -> Result<(), Error>;

    fn finish_edit(&mut self, command_id: ProcessId, edit_id: usize, data: Vec<u8>) -> Result<(), Error>;

    fn cancel(&mut self, id: ProcessId) -> Result<(), Error>;
}


impl EndpointExt for BackendEndpoint {
    fn cur_remote(&self) -> RemoteId {
        self.handler.remotes.last().unwrap().0
    }

    fn run(&mut self, c: Command, redirect: Option<String>) -> Result<ReadProcess, Error> {
        let cur_remote = self.cur_remote();

        let (stderr_read, stderr_write) = self.pipe();
        let (stdin_read, stdin_write) = self.pipe();
        if let Some(redirect) = redirect {
            let stdout_write = self.open_output_file(cur_remote, redirect)?;
            let (stdout_read, _) = self.pipe();
            let id = self.command(cur_remote, c, HashMap::new(), WritePipes {
                stdin: stdin_read, stdout: stdout_write, stderr: stderr_write
            })?;
            Ok(ReadProcess {
                id,
                pipes: ReadPipes {
                    stdin: stdin_write,
                    stdout: stdout_read,
                    stderr: stderr_read,
                },
            })
        } else {
            let (stdout_read, stdout_write) = self.pipe();
            let id = self.command(cur_remote, c, HashMap::new(), WritePipes {
                stdin: stdin_read, stdout: stdout_write, stderr: stderr_write
            })?;
            Ok(ReadProcess {
                id,
                pipes: ReadPipes {
                    stdin: stdin_write,
                    stdout: stdout_read,
                    stderr: stderr_read,
                },
            })
        }
    }

    fn begin_remote(&mut self, c: Command) -> Result<RemoteId, Error> {
        let cur_remote = self.cur_remote();
        let remote = self.remote(cur_remote, c)?;
        assert!(self.handler.waiting_for_remote.is_none());
        self.handler.waiting_for_remote = Some(remote);
        Ok(remote)
    }

    fn end_remote(&mut self) -> Result<(), Error> {
        let cur_remote = self.handler.remotes.pop().unwrap().0;
        Ok(self.close_remote(cur_remote)?)
    }

    fn finish_edit(&mut self, command_id: ProcessId, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        // let cur_remote = self.remotes.pop().unwrap();
        Ok(self.finish_edit(command_id, edit_id, data)?)
    }

    fn cancel(&mut self, id: ProcessId) -> Result<(), Error> {
        Ok(self.close_process(id)?)
    }
}
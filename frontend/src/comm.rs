
use std::sync::mpsc;
use std::process as pr;
use std::thread;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write, Read};
use std::io;

use failure::Error;
use os_pipe::{PipeWriter, IntoStdio};
use os_pipe;
use libc;
use serde_json;
use tempfile::tempdir;

use protocol::{ RpcResponse, Process, Multiplex, Command, Transport, Endpoint, RemoteId, EndpointHandler };

use Event;

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
    pub remotes: Vec<RemoteId>,
    pub waiting_for: Option<Process>,
}

impl<T: Transport> EndpointHandler<T> for StackedRemotes {
    fn pipe(_endpoint: &mut Endpoint<T, Self>, _id: usize, data: Vec<u8>) -> Result<(), Error> {
        Ok(io::stdout().write_all(&data)?)
    }

    fn command_done(endpoint: &mut Endpoint<T, Self>, id: usize, _exit_code: i64) -> Result<(), Error> {
        assert_eq!(endpoint.handler.waiting_for.expect("no process waiting").id, id);
        endpoint.handler.waiting_for = None;
        Ok(())
    }

    fn directory_listing(_endpoint: &mut Endpoint<T, Self>, _id: usize, _items: Vec<String>) -> Result<(), Error> {
        panic!();
    }

    fn edit_request(endpoint: &mut Endpoint<T, Self>, edit_id: usize, command_id: usize, name: String, data: Vec<u8>) -> Result<(), Error> {
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

pub fn launch_backend(sender: mpsc::Sender<Event>) -> Result<BackendEndpoint, Error> {

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

    let mut endpoint = Endpoint::new(
        PipeTransport { input: input_writer },
        StackedRemotes { remotes: vec![], waiting_for: None });

    let root = endpoint.root();
    endpoint.handler.remotes.push(root);

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

    Ok(endpoint)
}

pub trait EndpointExt {
    fn cur_remote(&self) -> RemoteId;

    fn run(&mut self, c: Command, redirect: Option<String>) -> Result<Process, Error>;

    fn begin_remote(&mut self, c: Command) -> Result<RemoteId, Error>;

    fn end_remote(&mut self) -> Result<(), Error>;

    fn finish_edit(&mut self, command_id: usize, edit_id: usize, data: Vec<u8>) -> Result<(), Error>;

    fn cancel(&mut self, id: usize) -> Result<(), Error>;
}


impl EndpointExt for BackendEndpoint {
    fn cur_remote(&self) -> RemoteId {
        *self.handler.remotes.last().unwrap()
    }

    fn run(&mut self, c: Command, redirect: Option<String>) -> Result<Process, Error> {
        let cur_remote = self.cur_remote();

        if let Some(redirect) = redirect {
            let handle = self.open_file(cur_remote, redirect)?;
            Ok(self.command(cur_remote, c, Some(handle))?)

        } else {
            Ok(self.command(cur_remote, c, None)?)
        }
    }

    fn begin_remote(&mut self, c: Command) -> Result<RemoteId, Error> {
        let cur_remote = self.cur_remote();
        let remote = self.remote(cur_remote, c)?;
        self.handler.remotes.push(remote);
        Ok(remote)
    }

    fn end_remote(&mut self) -> Result<(), Error> {
        let cur_remote = self.handler.remotes.pop().unwrap();
        Ok(self.close_remote(cur_remote)?)
    }

    fn finish_edit(&mut self, command_id: usize, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        // let cur_remote = self.remotes.pop().unwrap();
        Ok(self.finish_edit(command_id, edit_id, data)?)
    }

    fn cancel(&mut self, id: usize) -> Result<(), Error> {
        Ok(self.close_process(id)?)
    }
}
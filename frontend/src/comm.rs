
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

use protocol::{ RpcResponse, Process, Multiplex, Command, Transport, Endpoint, RemoteId };

use Event;

struct PipeTransport {
    input: PipeWriter,
}

impl Transport for PipeTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        self.input.write(data)?;
        self.input.flush()?;
        Ok(())
    }

}

pub struct BackendRemote {
    endpoint: Endpoint<PipeTransport>,
    pub remotes: Vec<RemoteId>,
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

    let endpoint = Endpoint::new(PipeTransport { input: input_writer });

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

    let root = endpoint.root();

    Ok(BackendRemote {
        endpoint,
        remotes: vec![root],
    })
}

impl BackendRemote {
    pub fn cur_remote(&self) -> RemoteId {
        *self.remotes.last().unwrap()
    }

    pub fn run(&mut self, c: Command) -> Result<Process, Error> {
        let cur_remote = self.cur_remote();
        Ok(self.endpoint.command(cur_remote, c)?)
    }

    pub fn begin_remote(&mut self, c: Command) -> Result<RemoteId, Error> {
        let cur_remote = self.cur_remote();
        let remote = self.endpoint.remote(cur_remote, c)?;
        self.remotes.push(remote);
        Ok(remote)
    }

    pub fn end_remote(&mut self) -> Result<(), Error> {
        let cur_remote = self.remotes.pop().unwrap();
        Ok(self.endpoint.close_remote(cur_remote)?)
    }

    pub fn cancel(&mut self, id: usize) -> Result<(), Error> {
        Ok(self.endpoint.close_process(id)?)
    }
}


use std::collections::{HashMap, HashSet};
use serde_json;

use crate::{
    RemoteId,
    ProcessId,
    WritePipe,
    GenericPipe,
    WriteProcess,
    ReadPipe,
    WritePipes,
    RemoteInfo,
    RemoteResponse,
    RemoteRequest,
    RemoteResponseEnvelope,
    RemoteRequestEnvelope,
    ProcessState,
    Request,
    Response,
    Condition,
    AbstractProcess,
    Command,
    RemoteState,
    Ids,
    PipeMessage,
    PipeEnvelope,
};

use failure::Error;

pub trait Transport {
    fn send(&mut self, msg: &[u8]) -> Result<(), Error>;
}

pub trait EndpointHandler<T: Transport>: Sized {
    fn remote_ready(endpoint: &mut Endpoint<T, Self>, id: RemoteId, remote_info: RemoteInfo) -> Result<(), Error>;
    fn command_done(endpoint: &mut Endpoint<T, Self>, id: ProcessId, exit_code: i64) -> Result<(), Error>;
    fn directory_listing(endpoint: &mut Endpoint<T, Self>, id: usize, items: Vec<String>) -> Result<(), Error>;
    fn edit_request(endpoint: &mut Endpoint<T, Self>, edit_id: usize, command_id: ProcessId, name: String, data: Vec<u8>) -> Result<(), Error>;
    fn pipe(endpoint: &mut Endpoint<T, Self>, id: GenericPipe, msg: PipeMessage) -> Result<(), Error>;
}

pub struct Endpoint<T: Transport, H: EndpointHandler<T>> {
    pub trans: T,
    pub handler: H,
    ids: Ids,
    remotes: HashMap<RemoteId, RemoteState>,
    jobs: HashMap<ProcessId, ProcessState>,
    pipes: HashSet<usize>,
}

fn ser_to_endpoint(remote: RemoteId, message: RemoteRequest) -> Vec<u8> {
    (serde_json::to_string(&Request {
        remote_id: remote.0,
        message: RemoteRequestEnvelope(message),
    }).unwrap() + "\n").into_bytes()
}

fn ser_to_frontend(remote: RemoteId, message: RemoteResponse) -> Vec<u8> {
    (serde_json::to_string(&Response {
        remote_id: remote.0,
        message: RemoteResponseEnvelope(message),
    }).unwrap() + "\n").into_bytes()
}

impl<T: Transport, H: EndpointHandler<T>> Endpoint<T, H> {
    pub fn new(trans: T, handler: H) -> Endpoint<T, H> {
        let mut remotes = HashMap::new();
        remotes.insert(RemoteId(0), RemoteState { parent: None });

        Endpoint {
            trans,
            handler,
            ids: Ids::new(),
            remotes,
            jobs: HashMap::new(),
            pipes: HashSet::new(),
        }
    }

    pub fn root(&self) -> RemoteId {
       RemoteId(0)
    }

    pub fn receive(&mut self, message: Response) -> Result<(), Error> {
        // eprintln!("msg: {:?}", message);
        match message.message.0 {
            RemoteResponse::RemoteReady { info } => {
                EndpointHandler::remote_ready(self, RemoteId(message.remote_id), info)
            }
            RemoteResponse::CommandDone { id, exit_code } => {
                EndpointHandler::command_done(self, id, exit_code)
            }
            RemoteResponse::DirectoryListing { id, items } => {
                EndpointHandler::directory_listing(self, id, items)
            }
            RemoteResponse::EditRequest { edit_id, command_id, name, data } => {
                EndpointHandler::edit_request(self, edit_id, command_id, name, data)
            }
            RemoteResponse::Pipe(pipe) => {
                assert!(self.pipes.contains(&pipe.id));
                EndpointHandler::pipe(self, GenericPipe(pipe.id), pipe.msg)
            }
        }
    }

    pub fn remote(&mut self, remote: RemoteId, command: Command) -> Result<RemoteId, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = self.ids.next();

        self.trans.send(&ser_to_endpoint(remote, RemoteRequest::BeginRemote {
            id,
            command,
        }))?;

        let old_state = self.remotes.insert(RemoteId(id), RemoteState {
            parent: Some(remote),
        });

        assert!(old_state.is_none());

        Ok(RemoteId(id))
    }

    pub fn pipe(&mut self) -> (ReadPipe, WritePipe) {
        let id = self.ids.next();
        self.pipes.insert(id);
        (ReadPipe(id), WritePipe(id))
    }

    pub fn command(&mut self, remote: RemoteId, command: Command, block_for: HashMap<ProcessId, Condition>, pipes: WritePipes) -> Result<ProcessId, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = ProcessId(self.ids.next());

        let process = AbstractProcess {
            id,
            stdin: pipes.stdin.0,
            stdout: pipes.stdout.0,
            stderr: pipes.stderr.0,
        };

        self.trans.send(&ser_to_endpoint(remote, RemoteRequest::BeginCommand {
            block_for,
            process,
            command,
        }))?;

        self.jobs.insert(id, ProcessState { parent: remote });

        Ok(id)
    }

    pub fn open_output_file(&mut self, remote: RemoteId, path: String) -> Result<WritePipe, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = self.ids.next();

        self.pipes.insert(id);

        self.trans.send(&ser_to_endpoint(remote, RemoteRequest::OpenOutputFile {
            id,
            path,
        }))?;

        Ok(WritePipe(id))
    }

    pub fn open_input_file(&mut self, remote: RemoteId, path: String) -> Result<ReadPipe, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = self.ids.next();

        self.pipes.insert(id);

        self.trans.send(&ser_to_endpoint(remote, RemoteRequest::OpenInputFile {
            id,
            path,
        }))?;

        Ok(ReadPipe(id))
    }


    pub fn close_remote(&mut self, remote: RemoteId) -> Result<(), Error> {
        let state = self.remotes.remove(&remote).expect("remote not connected");

        // TODO: close jobs?

        self.trans.send(&ser_to_endpoint(state.parent.expect("closing root remote"), RemoteRequest::EndRemote {
            id: remote.0,
        }))?;

        Ok(())
    }

    pub fn close_process(&mut self, id: ProcessId) -> Result<(), Error> {
        let process_state = self.jobs.remove(&id).expect("process not running");

        assert!(self.remotes.contains_key(&process_state.parent));

        self.trans.send(&ser_to_endpoint(process_state.parent, RemoteRequest::CancelCommand {
            id,
        }))?;

        Ok(())
    }

    pub fn finish_edit(&mut self, command_id: ProcessId, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        let process_state = self.jobs.get(&command_id).expect("process not running");

        assert!(self.remotes.contains_key(&process_state.parent));

        self.trans.send(&ser_to_endpoint(process_state.parent, RemoteRequest::FinishEdit {
            id: edit_id,
            data,
        }))?;

        Ok(())
    }

    pub fn pipe_read(&mut self, remote: RemoteId, id: ReadPipe, read_up_to: u64) -> Result<(), Error> {
        self.trans.send(&ser_to_endpoint(remote, RemoteRequest::Pipe(PipeEnvelope {
            id: id.0,
            msg: PipeMessage::Read {
                read_up_to,
            },
        })))?;

        Ok(())
    }

    pub fn pipe_begin_read(&mut self, remote: RemoteId, id: ReadPipe) -> Result<(), Error> {
        self.trans.send(&ser_to_endpoint(remote, RemoteRequest::Pipe(PipeEnvelope {
            id: id.0,
            msg: PipeMessage::BeginRead,
        })))?;

        Ok(())
    }

}

pub trait BackendHandler {
    fn begin_command(&mut self, block_for: HashMap<ProcessId, Condition>, process: WriteProcess, command: Command) -> Result<(), Error>;
    fn cancel_command(&mut self, id: ProcessId) -> Result<(), Error>;
    fn begin_remote(&mut self, id: usize, command: Command) -> Result<(), Error>;
    fn open_output_file(&mut self, id: WritePipe, path: String) -> Result<(), Error>;
    fn open_input_file(&mut self, id: ReadPipe, path: String) -> Result<(), Error>;
    fn end_remote(&mut self, id: usize) -> Result<(), Error>;
    fn list_directory(&mut self, id: usize, path: String) -> Result<(), Error>;
    fn finish_edit(&mut self, id: usize, data: Vec<u8>) -> Result<(), Error>;
    fn pipe(&mut self, id: GenericPipe, msg: PipeMessage) -> Result<(), Error>;
}

#[derive(Default)]
pub struct Backend<T: Transport> {
    pub trans: T,
}

impl Request {
    pub fn route<H: BackendHandler>(self, handler: &mut H) -> Result<(), Error> {
        // eprintln!("msg: {:?}", self);
        match self.message.0 {
            RemoteRequest::BeginCommand { block_for, process, command, } => {
                let process = WriteProcess {
                    id: process.id,
                    stdin: ReadPipe(process.stdin),
                    stdout: WritePipe(process.stdout),
                    stderr: WritePipe(process.stderr),
                };
                handler.begin_command(block_for, process, command)
            }
            RemoteRequest::CancelCommand { id, } => {
                handler.cancel_command(id)
            }
            RemoteRequest::BeginRemote { id, command, } => {
                handler.begin_remote(id, command)
            }
            RemoteRequest::OpenOutputFile { id, path, } => {
                handler.open_output_file(WritePipe(id), path)
            }
            RemoteRequest::OpenInputFile { id, path, } => {
                handler.open_input_file(ReadPipe(id), path)
            }
            RemoteRequest::EndRemote { id, } => {
                handler.end_remote(id)
            }
            RemoteRequest::ListDirectory { id, path, } => {
                handler.list_directory(id, path)
            }
            RemoteRequest::FinishEdit { id, data, } => {
                handler.finish_edit(id, data)
            }
            RemoteRequest::Pipe(pipe) => {
                handler.pipe(GenericPipe(pipe.id), pipe.msg)
            }
        }
    }
}

impl<T: Transport> Backend<T> {
    pub fn new(trans: T) -> Backend<T> {
        Backend {
            trans,
        }
    }

    pub fn remote_ready(&mut self, info: RemoteInfo) -> Result<(), Error> {
        self.trans.send(&ser_to_frontend(RemoteId(0), RemoteResponse::RemoteReady {
            info,
        }))?;

        Ok(())
    }

    pub fn pipe_data(&mut self, id: WritePipe, data: Vec<u8>, end_offset: u64) -> Result<(), Error> {
        self.trans.send(&ser_to_frontend(RemoteId(0), RemoteResponse::Pipe(PipeEnvelope {
            id: id.0,
            msg: PipeMessage::Data { data, end_offset, },
        })))?;

        Ok(())
    }

    pub fn pipe_closed(&mut self, id: WritePipe, end_offset: u64) -> Result<(), Error> {
        self.trans.send(&ser_to_frontend(RemoteId(0), RemoteResponse::Pipe(PipeEnvelope {
            id: id.0,
            msg: PipeMessage::Closed { end_offset },
        })))?;


        Ok(())
    }

    pub fn command_done(&mut self, id: ProcessId, exit_code: i64) -> Result<(), Error> {
        self.trans.send(&ser_to_frontend(RemoteId(0), RemoteResponse::CommandDone {
            id,
            exit_code,
        }))?;

        Ok(())
    }

    pub fn directory_listing(&mut self, id: usize, items: Vec<String>) -> Result<(), Error> {
        self.trans.send(&ser_to_frontend(RemoteId(0), RemoteResponse::DirectoryListing {
            id,
            items,
        }))?;

        Ok(())
    }

    pub fn edit_request(&mut self, command_id: ProcessId, edit_id: usize, name: String, data: Vec<u8>) -> Result<(), Error> {
        self.trans.send(&ser_to_frontend(RemoteId(0), RemoteResponse::EditRequest {
            command_id,
            edit_id,
            name,
            data,
        }))?;

        Ok(())
    }
}
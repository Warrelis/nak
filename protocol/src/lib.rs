#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate failure;

use std::collections::HashMap;

use failure::Error;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Unknown(String, Vec<String>),
    SetDirectory(String),
    Edit(String),
}

impl Command {
    pub fn add_args(&mut self, new_args: Vec<String>) {
        match self {
            &mut Command::Unknown(_, ref mut args) => {
                args.extend(new_args)
            }
            &mut Command::SetDirectory(_) |
            &mut Command::Edit(_) => panic!(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Multiplex<M> {
    pub remote_id: usize,
    pub message: M
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Process {
    pub id: usize,
    pub stdout_pipe: usize,
    pub stderr_pipe: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcRequest {
    BeginCommand {
        process: Process,
        command: Command,
    },
    CancelCommand {
        id: usize,
    },
    BeginRemote {
        id: usize,
        command: Command,
    },
    OpenFile {
        id: usize,
        path: String,
    },
    EndRemote {
        id: usize,
    },
    ListDirectory {
        id: usize,
        path: String,
    },
    FinishEdit {
        id: usize,
        data: Vec<u8>,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcResponse {
    // RemoteReady {
    //     id: usize,
    //     hostname: String,
    // },
    Pipe {
        id: usize,
        data: Vec<u8>,
    },
    CommandDone {
        id: usize,
        exit_code: i64,
    },
    DirectoryListing {
        id: usize,
        items: Vec<String>,
    },
    EditRequest {
        command_id: usize,
        edit_id: usize,
        name: String,
        data: Vec<u8>,
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct RemoteId(usize);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Handle(usize);

#[derive(Clone)]
struct RemoteState {
    parent: Option<RemoteId>,
}

pub trait Transport {
    fn send(&mut self, msg: &[u8]) -> Result<(), Error>;
}

struct Ids {
    next_id: usize
}

impl Ids {
    fn next_id(&mut self) -> usize {
        let res = self.next_id;
        self.next_id += 1;
        res
    }
}

struct ProcessState {
    parent: RemoteId,
    process: Process,
}

pub struct Endpoint<T: Transport> {
    trans: T,
    ids: Ids,
    remotes: HashMap<RemoteId, RemoteState>,
    jobs: HashMap<usize, ProcessState>,
}

fn ser_to_endpoint(remote: RemoteId, message: RpcRequest) -> Vec<u8> {
    (serde_json::to_string(&Multiplex {
        remote_id: remote.0,
        message,
    }).unwrap() + "\n").into_bytes()
}

impl<T: Transport> Endpoint<T> {
    pub fn new(trans: T) -> Endpoint<T> {
        let mut remotes = HashMap::new();
        remotes.insert(RemoteId(0), RemoteState { parent: None });

        Endpoint {
            trans,
            ids: Ids { next_id: 1 },
            remotes,
            jobs: HashMap::new(),
        }
    }

    pub fn root(&self) -> RemoteId {
       RemoteId(0)
    }

    pub fn remote(&mut self, remote: RemoteId, command: Command) -> Result<RemoteId, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = self.ids.next_id();

        self.trans.send(&ser_to_endpoint(remote, RpcRequest::BeginRemote {
            id,
            command,
        }))?;

        let old_state = self.remotes.insert(RemoteId(id), RemoteState {
            parent: Some(remote),
        });

        assert!(old_state.is_none());

        Ok(RemoteId(id))
    }

    pub fn command(&mut self, remote: RemoteId, command: Command, redirect: Option<Handle>) -> Result<Process, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = self.ids.next_id();
        let stdout_pipe = redirect.map(|h| h.0).unwrap_or_else(|| self.ids.next_id());
        let stderr_pipe = self.ids.next_id();

        let process = Process {
            id,
            stdout_pipe,
            stderr_pipe,
        };

        self.trans.send(&ser_to_endpoint(remote, RpcRequest::BeginCommand {
            process,
            command,
        }))?;

        self.jobs.insert(id, ProcessState { process, parent: remote });

        Ok(process)
    }

    pub fn open_file(&mut self, remote: RemoteId, path: String) -> Result<Handle, Error> {
        assert!(self.remotes.contains_key(&remote));

        let id = self.ids.next_id();

        self.trans.send(&ser_to_endpoint(remote, RpcRequest::OpenFile {
            id,
            path,
        }))?;

        Ok(Handle(id))
    }

    pub fn close_remote(&mut self, remote: RemoteId) -> Result<(), Error> {
        let state = self.remotes.remove(&remote).expect("remote not connected");

        // TODO: close jobs?

        self.trans.send(&ser_to_endpoint(state.parent.expect("closing root remote"), RpcRequest::EndRemote {
            id: remote.0,
        }))?;

        Ok(())
    }

    pub fn close_process(&mut self, id: usize) -> Result<(), Error> {
        let process_state = self.jobs.remove(&id).expect("process not running");

        assert!(self.remotes.contains_key(&process_state.parent));

        self.trans.send(&ser_to_endpoint(process_state.parent, RpcRequest::CancelCommand {
            id,
        }))?;

        Ok(())
    }

    pub fn finish_edit(&mut self, command_id: usize, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        let process_state = self.jobs.get(&command_id).expect("process not running");

        assert!(self.remotes.contains_key(&process_state.parent));

        self.trans.send(&ser_to_endpoint(process_state.parent, RpcRequest::FinishEdit {
            id: edit_id,
            data,
        }))?;

        Ok(())
    }
}

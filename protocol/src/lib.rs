#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate failure;

mod comm;

use std::collections::HashMap;

pub use crate::comm::{
    EndpointHandler,
    Endpoint,
    BackendHandler,
    Backend,
    Transport,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Unknown(String, Vec<String>),
    SetDirectory(String),
    GetDirectory,
    Edit(String),
}

impl Command {
    pub fn add_args(&mut self, new_args: Vec<String>) {
        match self {
            &mut Command::Unknown(_, ref mut args) => {
                args.extend(new_args)
            }
            &mut Command::SetDirectory(_) |
            &mut Command::GetDirectory |
            &mut Command::Edit(_) => panic!(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub remote_id: usize,
    pub message: RemoteResponseEnvelope,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub remote_id: usize,
    pub message: RemoteRequestEnvelope,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ProcessId(usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ExitStatus {
    Success,
    Failure,
}

impl ExitStatus {
    pub fn from_exit_code(code: i64) -> ExitStatus {
        if code == 0 {
            ExitStatus::Success
        } else {
            ExitStatus::Failure
        }
    }
}

pub type Condition = Option<ExitStatus>;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ReadPipe(usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct WritePipe(usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct GenericPipe(usize);

impl ReadPipe {
    pub fn to_generic(&self) -> GenericPipe {
        GenericPipe(self.0)
    }
}

impl WritePipe {
    pub fn to_generic(&self) -> GenericPipe {
        GenericPipe(self.0)
    }
}

impl GenericPipe {
    pub fn to_write(&self) -> WritePipe {
        WritePipe(self.0)
    }

    pub fn to_read(&self) -> ReadPipe {
        ReadPipe(self.0)
    }
}

pub struct Testing {
    ids: Ids,
}

impl Testing {
    pub fn new() -> Testing {
        Testing {
            ids: Ids::new(),
        }
    }

    pub fn pipe(&mut self) -> (ReadPipe, WritePipe) {
        let id = self.ids.next();

        (ReadPipe(id), WritePipe(id))
    }

    pub fn process(&mut self) -> ProcessId {
        ProcessId(self.ids.next())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadPipes {
    pub stdin: WritePipe,
    pub stdout: ReadPipe,
    pub stderr: ReadPipe,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WritePipes {
    pub stdin: ReadPipe,
    pub stdout: WritePipe,
    pub stderr: WritePipe,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WriteProcess {
    pub id: ProcessId,
    pub pipes: WritePipes,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadProcess {
    pub id: ProcessId,
    pub pipes: ReadPipes,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AbstractProcess {
    id: ProcessId,
    stdin: usize,
    stdout: usize,
    stderr: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteRequestEnvelope(RemoteRequest);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteResponseEnvelope(RemoteResponse);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PipeMessage {
    BeginRead,
    Data {
        data: Vec<u8>,
        end_offset: u64,
    },
    Read {
        read_up_to: u64,
    },
    Closed {
        end_offset: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PipeEnvelope<Id> {
    id: Id,
    msg: PipeMessage
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum RemoteRequest {
    BeginCommand {
        block_for: HashMap<ProcessId, Condition>,
        process: AbstractProcess,
        command: Command,
    },
    CancelCommand {
        id: ProcessId,
    },
    BeginRemote {
        id: usize,
        command: Command,
    },
    OpenOutputFile {
        id: usize,
        path: String,
    },
    OpenInputFile {
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
    },
    Pipe(PipeEnvelope<usize>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RemoteResponse {
    RemoteReady {
        info: RemoteInfo,
    },
    CommandDone {
        id: ProcessId,
        exit_code: i64,
    },
    DirectoryListing {
        id: usize,
        items: Vec<String>,
    },
    EditRequest {
        command_id: ProcessId,
        edit_id: usize,
        name: String,
        data: Vec<u8>,
    },
    Pipe(PipeEnvelope<usize>),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct RemoteId(usize);

#[derive(Clone)]
struct RemoteState {
    parent: Option<RemoteId>,
}

pub struct Ids {
    next_id: usize
}

impl Ids {
    pub fn new() -> Ids {
        Ids {
            next_id: 0,
        }
    }

    pub fn next(&mut self) -> usize {
        let res = self.next_id;
        self.next_id += 1;
        res
    }
}

struct ProcessState {
    parent: RemoteId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteInfo {
    pub hostname: String,
    pub username: String,
    pub working_dir: String,
}


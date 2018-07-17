#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Unknown(String, Vec<String>),
    SetDirectory(String),
}

impl Command {
    pub fn add_args(&mut self, new_args: Vec<String>) {
        match self {
            &mut Command::Unknown(_, ref mut args) => {
                args.extend(new_args)
            }
            &mut Command::SetDirectory(_) => panic!(), 
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Multiplex<M> {
    pub remote_id: usize,
    pub message: M
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcRequest {
    BeginCommand {
        id: usize,
        stdout_pipe: usize,
        stderr_pipe: usize,
        command: Command,
    },
    CancelCommand {
        id: usize,
    },
    BeginRemote {
        id: usize,
        command: Command,
    },
    EndRemote {
        id: usize,
    },
    ListDirectory {
        id: usize,
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcResponse {
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
    }
}

#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate failure;
extern crate os_pipe;

use std::io::Write;
use std::io::Read;
use std::io;

use failure::Error;
use std::process as pr;
use os_pipe::IntoStdio;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Tool {
    Path(String),
    Remote {
        host: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Args {
    args: Vec<String>
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Command {
    tool: Tool,
    args: Args,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcRequest {
    BeginCommand {
        id: usize,
        command: Command,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcResponse {
    CommandDone {
        id: usize,
        output: String,
        exit_code: i64,
    }
}

pub struct Backend;

impl Backend {
    fn run(&mut self, c: Command) -> Result<(String, i64), Error> {
        match c.tool {
            Tool::Path(path) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&c.args.args);

                let (mut reader, writer) = os_pipe::pipe()?;

                cmd.stdout(writer.into_stdio());

                let mut handle = cmd.spawn()?;

                drop(cmd);

                let mut output = String::new();

                reader.read_to_string(&mut output)?;
                let exit_code = handle.wait()?.code().unwrap_or(-1);

                Ok((output, exit_code.into()))
            }
            Tool::Remote { .. } => panic!(),
        }
    }
}

fn main() {
    let mut backend = Backend;

    loop {
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(n) => {
                if n == 0 {
                    break;
                }

                let rpc: RpcRequest = serde_json::from_str(&input).unwrap();

                match rpc {
                    RpcRequest::BeginCommand { id, command } => {
                        let (output, exit_code) = backend.run(command).unwrap();
                        let serialized = serde_json::to_string(&RpcResponse::CommandDone {
                            id,
                            output,
                            exit_code,
                        }).unwrap();

                        write!(io::stdout(), "{}\n", serialized).unwrap();
                    }
                }
            }
            Err(error) => {
                eprintln!("error: {}", error);
                break;
            }
        }
    }
}

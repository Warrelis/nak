#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate failure;
extern crate os_pipe;

use std::io::Write;
use std::io::Read;
use std::io;
use std::env;

use failure::Error;
use std::process as pr;
use os_pipe::IntoStdio;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Unknown(String, Args),
    SetDirectory(String),
    Remote {
        host: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Args {
    args: Vec<String>
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcRequest {
    BeginCommand {
        id: usize,
        stdout_pipe: usize,
        stderr_pipe: usize,
        command: Command,
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
}

fn write_pipe(id: usize, data: Vec<u8>) -> Result<(), Error> {
    write!(io::stdout(), "{}\n", serde_json::to_string(&RpcResponse::Pipe {
        id,
        data,
    })?)?;
    Ok(())
}

fn write_done(id: usize, exit_code: i64) -> Result<(), Error> {
    write!(io::stdout(), "{}\n", serde_json::to_string(&RpcResponse::CommandDone {
        id,
        exit_code,
    })?)?;
    Ok(())
}

pub struct Backend;

impl Backend {
    fn run(&mut self, id: usize, stdout_pipe: usize, stderr_pipe: usize, c: Command) -> Result<(), Error> {
        match c {
            Command::Unknown(path, args) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&args.args);

                let (mut output_reader, output_writer) = os_pipe::pipe()?;
                let (mut error_reader, error_writer) = os_pipe::pipe()?;

                cmd.stdout(output_writer.into_stdio());
                cmd.stderr(error_writer.into_stdio());

                let mut handle = cmd.spawn()?;

                drop(cmd);

                let mut buf = [0u8; 1024];

                loop {
                    let len = output_reader.read(&mut buf)?;
                    if len == 0 {
                        break;
                    }
                    write_pipe(stdout_pipe, buf[..len].to_vec())?;
                }

                let exit_code = handle.wait()?.code().unwrap_or(-1);

                write_done(id, exit_code.into())
            }
            Command::SetDirectory(dir) => {
                match env::set_current_dir(dir) {
                    Ok(_) => write_done(id, 0),
                    Err(e) => {
                        write_pipe(stdout_pipe, format!("Error: {:?}", e).into_bytes())?;
                        write_done(id, 1)
                    }
                }

                
            },
            Command::Remote { .. } => panic!(),
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
                    RpcRequest::BeginCommand { id, stdout_pipe, stderr_pipe, command } => {
                        backend.run(id, stdout_pipe, stderr_pipe, command).unwrap();
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

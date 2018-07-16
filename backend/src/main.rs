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
pub enum Rpc {
    BeginCommand {
        id: usize,
        command: Command,
    },
    CommandResult {
        id: usize,
        output: String,
    }
}

pub struct Backend;

impl Backend {
    fn run(&mut self, c: Command) -> Result<String, Error> {
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
                handle.wait()?;

                Ok(output)
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

                let rpc: Rpc = serde_json::from_str(&input).unwrap();

                match rpc {
                    Rpc::BeginCommand { id, command } => {
                        let output = backend.run(command).unwrap();
                        let serialized = serde_json::to_string(&Rpc::CommandResult {
                            id,
                            output,
                        }).unwrap();

                        write!(io::stdout(), "{}\n", serialized).unwrap();
                    }
                    Rpc::CommandResult { .. } => panic!(),
                }
            }
            Err(error) => {
                eprintln!("error: {}", error);
                break;
            }
        }
    }
}

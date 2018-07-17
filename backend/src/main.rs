#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate failure;
extern crate os_pipe;
extern crate ctrlc;
extern crate protocol;

use std::collections::HashMap;
use std::io::{Write, Read, BufRead, BufReader};
use std::{io, env, fs, thread};
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::Arc;
use std::sync::atomic::{Ordering, AtomicBool};

use failure::Error;
use std::process as pr;
use os_pipe::IntoStdio;
use os_pipe::PipeWriter;

use protocol::{Multiplex, RpcResponse, RpcRequest, Command, Process};

fn write_pipe(id: usize, data: Vec<u8>) -> Result<(), Error> {
    write!(io::stdout(), "{}\n", serde_json::to_string(&Multiplex {
        remote_id: 0,
        message: RpcResponse::Pipe {
            id,
            data,
        },
    })?)?;
    io::stdout().flush().unwrap();
    Ok(())
}

fn write_done(id: usize, exit_code: i64) -> Result<(), Error> {
    write!(io::stdout(), "{}\n", serde_json::to_string(&Multiplex {
        remote_id: 0,
        message: RpcResponse::CommandDone {
            id,
            exit_code,
        },
    })?)?;
    io::stdout().flush().unwrap();
    Ok(())
}

fn write_directory_listing(id: usize, items: Vec<String>) -> Result<(), Error> {
    write!(io::stdout(), "{}\n", serde_json::to_string(&Multiplex {
        remote_id: 0,
        message: RpcResponse::DirectoryListing {
            id,
            items,
        },
    })?)?;
    io::stdout().flush().unwrap();
    Ok(())
}

pub struct BackendRemote {
    backend_id: usize,
    next_id: usize,
    shutting_down: Arc<AtomicBool>,
    handle: pr::Child,
    input: PipeWriter,
}

#[derive(Default)]
pub struct Backend {
    subbackends: HashMap<usize, BackendRemote>,
    jobs: HashMap<usize, mpsc::Sender<()>>,
}

impl Backend {
    fn run(&mut self, id: usize, stdout_pipe: usize, stderr_pipe: usize, c: Command) -> Result<(), Error> {
        match c {
            Command::Unknown(path, args) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&args);

                let (mut output_reader, output_writer) = os_pipe::pipe()?;
                let (mut error_reader, error_writer) = os_pipe::pipe()?;

                cmd.stdout(output_writer.into_stdio());
                cmd.stderr(error_writer.into_stdio());

                let handle = Arc::new(Mutex::new(cmd.spawn()?));
                let cancel_handle = handle.clone();

                drop(cmd);

                let (cancel_send, cancel_recv) = mpsc::channel();

                self.jobs.insert(id, cancel_send);

                thread::spawn(move || {
                    cancel_recv.recv().unwrap();
                    eprintln!("{} received cancel", id);

                    // handle.lock().unwrap().kill().unwrap();

                    eprintln!("{} finished cancel", id);
                });

                thread::spawn(move || {
                    let mut buf = [0u8; 1024];

                    loop {
                        let len = output_reader.read(&mut buf).unwrap();
                        eprintln!("{} read stdout {:?}", id, len);
                        if len == 0 {
                            break;
                        }
                        write_pipe(stdout_pipe, buf[..len].to_vec()).unwrap();
                    }

                    loop {
                        let len = error_reader.read(&mut buf).unwrap();
                        eprintln!("{} read stderr {:?}", id, len);
                        if len == 0 {
                            break;
                        }
                        write_pipe(stderr_pipe, buf[..len].to_vec()).unwrap();
                    }

                    let exit_code = cancel_handle.lock().unwrap().wait().unwrap().code().unwrap_or(-1);
                    eprintln!("{} exit {}", id, exit_code);

                    write_done(id, exit_code.into()).unwrap();
                });

                Ok(())
            }
            Command::SetDirectory(dir) => {
                match env::set_current_dir(dir) {
                    Ok(_) => write_done(id, 0),
                    Err(e) => {
                        write_pipe(stdout_pipe, format!("Error: {:?}", e).into_bytes())?;
                        write_done(id, 1)
                    }
                }
            }
        }
    }

    fn begin_remote(&mut self, id: usize, c: Command) -> Result<(), Error> {
        match c {
            Command::Unknown(path, args) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&args);

                let (output_reader, output_writer) = os_pipe::pipe()?;
                let (input_reader, input_writer) = os_pipe::pipe()?;

                cmd.stdout(output_writer.into_stdio());
                cmd.stdin(input_reader.into_stdio());

                let handle = cmd.spawn()?;

                drop(cmd);

                let mut output = BufReader::new(output_reader);

                let mut input = String::new();
                output.read_line(&mut input)?;
                assert_eq!(input, "nxQh6wsIiiFomXWE+7HQhQ==\n");

                let shutting_down = Arc::new(AtomicBool::new(false));
                let shutting_down_clone = shutting_down.clone();

                thread::spawn(move || {
                    loop {
                        let mut input = String::new();
                        match output.read_line(&mut input) {
                            Ok(n) => {
                                if n == 0 {
                                    break;
                                }

                                let mut rpc: Multiplex<RpcResponse> = serde_json::from_str(&input).unwrap();
                                rpc.remote_id = id;

                                write!(io::stdout(), "{}\n", serde_json::to_string(&rpc).unwrap()).unwrap();
                            }
                            Err(error) => {
                                if !shutting_down_clone.load(Ordering::SeqCst) {
                                    eprintln!("error: {}", error)
                                }
                            }
                        }
                    }
                });

                self.subbackends.insert(id, BackendRemote {
                    backend_id: id,
                    shutting_down,
                    handle,
                    next_id: 0,
                    input: input_writer,
                });

                Ok(())
            }
            _ => panic!(),
        }
    }

    fn end_remote(&mut self, id: usize) -> Result<(), Error> {
        let mut backend = self.subbackends.remove(&id).unwrap();
        backend.shutting_down.store(true, Ordering::SeqCst);
        backend.handle.kill()?;
        Ok(())
    }

    fn cancel(&mut self, id: usize) -> Result<(), Error> {
        self.jobs.get(&id).unwrap().send(())?;
        Ok(())
    }
}

fn main() {
    let mut backend = Backend::default();

    ctrlc::set_handler(move || {
        eprintln!("backend caught CtrlC");
    }).expect("Error setting CtrlC handler");

    // eprintln!("spawn_self");
    write!(io::stdout(), "nxQh6wsIiiFomXWE+7HQhQ==\n").unwrap();
    io::stdout().flush().unwrap();

    loop {
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(n) => {
                if n == 0 {
                    break;
                }
                // eprintln!("part {}", input);

                let rpc: Multiplex<RpcRequest> = serde_json::from_str(&input).unwrap();

                if rpc.remote_id == 0 {
                    match rpc.message {
                        RpcRequest::BeginCommand { process: Process { id, stdout_pipe, stderr_pipe } , command } => {
                            backend.run(id, stdout_pipe, stderr_pipe, command).unwrap();
                        }
                        RpcRequest::BeginRemote { id, command } => {
                            backend.begin_remote(id, command).unwrap();
                        }
                        RpcRequest::EndRemote { id } => {
                            backend.end_remote(id).unwrap();
                        }
                        RpcRequest::CancelCommand { id } => {
                            // eprintln!("caught CtrlC");
                            backend.cancel(id).unwrap();
                        }
                        RpcRequest::ListDirectory { id, path } => {
                            let items = fs::read_dir(path).unwrap().map(|i| {
                                i.unwrap().file_name().into_string().unwrap()
                            }).collect();

                            write_directory_listing(id, items).unwrap();
                        }
                    }
                } else {
                    let input = serde_json::to_string(&Multiplex {
                        remote_id: 0,
                        message: rpc.message,
                    }).unwrap();
                    // eprintln!("write {} {}", rpc.remote_id - 1, input);
                    let pipe = &mut backend.subbackends.get_mut(&rpc.remote_id).unwrap().input;
                    write!(
                        pipe,
                        "{}\n",
                        input).unwrap();
                    pipe.flush().unwrap();
                }
            }
            Err(error) => {
                eprintln!("error: {}", error);
                break;
            }
        }
    }
}

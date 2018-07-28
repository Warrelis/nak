#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate failure;
extern crate os_pipe;

extern crate base64;
extern crate rand;
extern crate protocol;

extern crate futures;
extern crate tokio;
extern crate hostname;

#[cfg(unix)] extern crate ctrlc;
#[cfg(unix)] extern crate unix_socket;
#[cfg(unix)] extern crate nix;

mod machine;
mod exec;

#[cfg(unix)] mod run;

#[cfg(unix)] use run::run_experiment;

use std::collections::HashMap;
use std::io::{Write, Read, BufRead, BufReader, Seek, SeekFrom};
use std::{io, env, fs, thread};
use std::fs::OpenOptions;
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::Arc;
use std::sync::atomic::{Ordering, AtomicBool};

use failure::Error;
use std::process as pr;
use os_pipe::IntoStdio;
use os_pipe::PipeWriter;
// use tokio::prelude::*;
// use tokio::io::AsyncRead;

#[cfg(unix)]
use unix_socket::{UnixStream, UnixListener};

use rand::{Rng, thread_rng};

use protocol::{
    Response,
    Request,
    Command,
    WriteProcess,
    WritePipe,
    ReadPipe,
    BackendHandler,
    Transport,
    Condition,
    ProcessId,
    ExitStatus,
    Backend,
    RemoteInfo,
};

use exec::{Exec, RunCmd};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRequest {
    file_name: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditResponse {
    data: Vec<u8>,
}

pub struct BackendRemote {
    shutting_down: Arc<AtomicBool>,
    handle: pr::Child,
    input: PipeWriter,
}

#[derive(Clone)]
pub struct CommandInfo {
    remote_id: usize,
    id: ProcessId,
}

#[derive(Default)]
pub struct StdoutTransport;

impl Transport for StdoutTransport {
    fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut stdout = io::stdout();
        stdout.write(data)?;
        stdout.flush()?;
        Ok(())
    }
}

pub struct ExecHandler {
    backtraffic: Arc<Mutex<Backend<StdoutTransport>>>,
}

impl exec::Handler for ExecHandler {
    fn pipe_output(&mut self, pipe: WritePipe, data: Vec<u8>) -> Result<(), Error> {
        self.backtraffic.lock().unwrap().pipe(pipe, data)
    }

    fn command_result(&mut self, pid: ProcessId, exit_code: i64) -> Result<(), Error> {
        self.backtraffic.lock().unwrap().command_done(pid, exit_code)
    }

    fn edit_request(&mut self, pid: ProcessId, edit_id: usize, path: String, data: Vec<u8>) -> Result<(), Error> {
        self.backtraffic.lock().unwrap().edit_request(pid, edit_id, path, data)
    }
}

pub struct AsyncBackendHandler {
    backtraffic: Arc<Mutex<Backend<StdoutTransport>>>,
    running_commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
    waiting_edits: Arc<Mutex<HashMap<usize, mpsc::Sender<Vec<u8>>>>>,
    subbackends: HashMap<usize, BackendRemote>,
    exec: Exec,
}

impl AsyncBackendHandler {
    fn new() -> AsyncBackendHandler {
        let backtraffic = Arc::new(Mutex::new(Backend::new(StdoutTransport)));
        let exec = Exec::new(Box::new(ExecHandler {
            backtraffic: backtraffic.clone(),
        }));
        AsyncBackendHandler {
            backtraffic,
            running_commands: Default::default(),
            waiting_edits: Default::default(),
            subbackends: Default::default(),
            exec,
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

                                let mut rpc: Response = serde_json::from_str(&input).unwrap();
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
                    shutting_down,
                    handle,
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
}

impl BackendHandler for AsyncBackendHandler {
    fn begin_command(&mut self, block_for: HashMap<ProcessId, Condition>, process: WriteProcess, command: Command) -> Result<(), Error> {
        let cmd = RunCmd {
            cmd: command,
            stdin: process.stdin,
            stdout: process.stdout,
            stderr: process.stderr,
        };
        self.exec.enqueue(process.id, cmd, block_for)?;
        Ok(())
    }

    fn cancel_command(&mut self, id: ProcessId) -> Result<(), Error> {
        self.exec.cancel(id)
    }

    fn begin_remote(&mut self, id: usize, command: Command) -> Result<(), Error> {
        self.begin_remote(id, command)
    }

    fn open_output_file(&mut self, id: WritePipe, path: String) -> Result<(), Error> {
        self.exec.open_output_file(id, path)
    }

    fn open_input_file(&mut self, id: ReadPipe, path: String) -> Result<(), Error> {
        self.exec.open_input_file(id, path)
    }

    fn end_remote(&mut self, id: usize) -> Result<(), Error> {
        self.end_remote(id)
    }

    fn list_directory(&mut self, id: usize, path: String) -> Result<(), Error> {
        let items = fs::read_dir(path).unwrap().map(|i| {
            i.unwrap().file_name().into_string().unwrap()
        }).collect();

        self.backtraffic.lock().unwrap().directory_listing(id, items)?;
        Ok(())
    }

    fn finish_edit(&mut self, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        self.exec.finish_edit(edit_id, data)?;
        Ok(())
    }

    fn pipe_data(&mut self, _id: usize, _data: Vec<u8>) -> Result<(), Error> {
        panic!();
    }

    fn pipe_read(&mut self, _id: usize, _count_bytes: usize) -> Result<(), Error> {
        panic!();
    }

}

fn random_key() -> String {
    let mut bytes = [0u8; 16];
    thread_rng().fill(&mut bytes);
    base64::encode_config(&bytes, base64::URL_SAFE)
}

#[cfg(unix)]
fn setup_editback_socket(
    socket_path: &str, 
    backtraffic: Arc<Mutex<Backend<StdoutTransport>>>,
    running_commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
    waiting_edits: Arc<Mutex<HashMap<usize, mpsc::Sender<Vec<u8>>>>>) -> Result<(), Error>
{
    let listener = UnixListener::bind(socket_path).expect("bind listen socket");

    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let running_commands = running_commands.clone();
                    let waiting_edits = waiting_edits.clone();
                    let backtraffic = backtraffic.clone();
                    thread::spawn(move || {

                        let (sender, receiver) = mpsc::channel();
                        {
                            let mut reader = BufReader::new(&mut stream);

                            let mut key = String::new();
                            reader.read_line(&mut key).unwrap();
                            assert_eq!(&key[key.len()-1..], "\n");
                            key.pop();

                            let command_info = running_commands.lock().unwrap().get(&key).cloned().expect("key not found");

                            let mut line = String::new();
                            reader.read_line(&mut line).unwrap();

                            let req: EditRequest = serde_json::from_str(&line).unwrap();

                            waiting_edits.lock().unwrap().insert(0, sender);
                            backtraffic.lock().unwrap().edit_request(command_info.id, 0, req.file_name, req.data).unwrap();
                        }

                        let msg = EditResponse {
                            data: receiver.recv().unwrap(),
                        };

                        stream.write((serde_json::to_string(&msg).unwrap() + "\n").as_bytes()).unwrap();
                    });
                }
                Err(err) => {
                    eprintln!("error: {:?}", err);
                    break;
                }
            }
        }

        // close the listener socket
        drop(listener);
    });
    Ok(())
}


#[cfg(not(unix))]
fn setup_editback_socket(
    socket_path: &str, 
    backtraffic: Arc<Mutex<Backend<StdoutTransport>>>,
    running_commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
    waiting_edits: Arc<Mutex<HashMap<usize, mpsc::Sender<Vec<u8>>>>>) -> Result<(), Error>
{
    Ok(())
}


#[cfg(unix)]
fn setup_ctrlc_handler() {
    ctrlc::set_handler(move || {
        eprintln!("backend caught CtrlC");
    }).expect("Error setting CtrlC handler");
}

#[cfg(not(unix))]
fn setup_ctrlc_handler() {

}

fn run_backend() -> Result<(), Error> {

    let mut backend = AsyncBackendHandler::new();

    setup_ctrlc_handler();

    // eprintln!("spawn_self");
    write!(io::stdout(), "nxQh6wsIiiFomXWE+7HQhQ==\n").unwrap();
    io::stdout().flush().unwrap();

    let socket_path = format!("/tmp/nak-backend-{}", random_key());

    eprintln!("socket_path: {:?}", socket_path);
    env::set_var("NAK_SOCKET_PATH", &socket_path);
    env::set_var("PAGER", "nak-backend pager");
    env::set_var("EDITOR", format!("{} editor", env::current_exe()?.to_str().expect("current exe")));

    setup_editback_socket(
        &socket_path,
        backend.backtraffic.clone(),
        backend.running_commands.clone(),
        backend.waiting_edits.clone())?;

    let hostname = hostname::get_hostname().unwrap();
    let username = String::from("dummyuser");
    let working_dir = env::current_dir().unwrap().to_str().unwrap().to_string();

    backend.backtraffic.lock().unwrap().remote_ready(RemoteInfo {
        hostname,
        username,
        working_dir,
    })?;

    loop {
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(n) => {
                if n == 0 {
                    break;
                }
                // eprintln!("part {}", input);

                let rpc: Request = serde_json::from_str(&input)?;

                if rpc.remote_id == 0 {
                    rpc.route(&mut backend).unwrap();
                } else {
                    let input = serde_json::to_string(&Request {
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

    Ok(())
}

#[cfg(unix)]
fn run_pager() -> Result<(), Error> {
    let socket_path = env::var("NAK_SOCKET_PATH").unwrap();
    let command_key = env::var("NAK_COMMAND_KEY").unwrap();


    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all((command_key + "\n").as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    eprintln!("pager_response {}", response);
    Ok(())
}

#[cfg(not(unix))]
fn run_pager() -> Result<(), Error> {
    unimplemented!();
}

#[cfg(unix)]
fn run_editor(path: &str) -> Result<(), Error> {
    let mut contents = Vec::new();
    let mut handle = OpenOptions::new().read(true).write(true).open(path)?;
    handle.read_to_end(&mut contents)?;
    handle.seek(SeekFrom::Start(0)).unwrap();

    let socket_path = env::var("NAK_SOCKET_PATH").unwrap();
    let command_key = env::var("NAK_COMMAND_KEY").unwrap();

    let mut stream = UnixStream::connect(socket_path).unwrap();
    
    stream.write_all((command_key + "\n").as_bytes()).unwrap();

    stream.write_all((serde_json::to_string(&EditRequest {
        file_name: path.to_string(),
        data: contents,
    }).unwrap() + "\n").as_bytes()).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    let resp: EditResponse = serde_json::from_str(&response).unwrap();

    handle.set_len(0).unwrap();
    handle.write_all(&resp.data).unwrap();

    Ok(())
}

#[cfg(not(unix))]
fn run_editor(path: &str) -> Result<(), Error> {
    unimplemented!();
}

#[cfg(not(unix))]
fn run_experiment() -> Result<(), Error> {
    unimplemented!();
}

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();

    if args.len() == 1 {
        run_backend()
    } else if args.len() == 2 && args[1] == "pager" {
        run_pager()
    } else if args.len() == 3 && args[1] == "editor" {
        run_editor(&args[2])
    } else if args.len() == 2 && args[1] == "experiment" {
        run_experiment()
    } else {
        assert!(false);
        Err(format_err!("o.O?"))
    }
}

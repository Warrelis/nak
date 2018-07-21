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

#[cfg(unix)] mod run;

#[cfg(unix)] use run::run_experiment;

use std::collections::HashMap;
use std::io::{Write, Read, BufRead, BufReader, Seek, SeekFrom};
use std::{io, env, fs, thread};
use std::fs::{File, OpenOptions};
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
    ReadPipe,
    WritePipe,
    BackendHandler,
    Transport,
    Condition,
    ProcessId,
    ExitStatus,
    Backend,
    RemoteInfo,
};

use machine::{Machine, Task, Status};

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

struct RunCmd {
    cmd: Command,
    stdin: ReadPipe,
    stdout: WritePipe,
    stderr: WritePipe,
}

enum ProcessState {
    Running {
        cancel: mpsc::Sender<()>,
    },
    AwaitingEdit,
}

enum RunResult {
    Process(ProcessState),
    AlreadyDone(ExitStatus),
}

pub struct AsyncBackendHandler {
    backtraffic: Arc<Mutex<Backend<StdoutTransport>>>,
    running_commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
    waiting_edits: Arc<Mutex<HashMap<usize, mpsc::Sender<Vec<u8>>>>>,
    subbackends: HashMap<usize, BackendRemote>,
    open_files: HashMap<WritePipe, File>,
    machine: Machine<ProcessId, RunCmd, ProcessState>,
}

impl AsyncBackendHandler {
    fn new() -> AsyncBackendHandler {
        AsyncBackendHandler {
            backtraffic: Arc::new(Mutex::new(Backend::new(StdoutTransport))),
            running_commands: Default::default(),
            waiting_edits: Default::default(),
            subbackends: Default::default(),
            open_files: Default::default(),
            machine: Machine::new(),
        }
    }

    fn enqueue(&mut self, id: ProcessId, cmd: RunCmd, block_for: HashMap<ProcessId, Condition>) -> Result<(), Error> {
        let tasks = self.machine.enqueue(id, cmd, block_for);

        for t in tasks {
            match t {
                Task::Start(pid, cmd) => {
                    match self.run(pid, cmd.stdout, cmd.stderr, cmd.cmd) {
                        Ok(RunResult::Process(state)) => self.machine.start(pid, state),
                        Ok(RunResult::AlreadyDone(status)) => self.machine.start_completed(pid, status),
                        Err(e) => {
                            self.machine.start_completed(pid, ExitStatus::Failure);

                            let mut back = self.backtraffic.lock().unwrap();
                            let text = format!("{:?}", e);
                            back.pipe(cmd.stderr, text.into_bytes())?;
                            back.command_done(pid, 1)?;
                        }
                    }
                }
                Task::ConditionFailed(pid, _) => {
                    self.backtraffic.lock().unwrap().command_done(pid, 1).unwrap();
                }
            }
        }

        Ok(())
    }

    fn run(&mut self, id: ProcessId, stdout_pipe: WritePipe, stderr_pipe: WritePipe, c: Command) -> Result<RunResult, Error> {
        eprintln!("{:?} running {:?}", id, c);
        match c {
            Command::Unknown(path, args) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&args);

                let command_key = random_key();
                cmd.env("NAK_COMMAND_KEY", &command_key);
                self.running_commands.lock().unwrap().insert(command_key.clone(), CommandInfo {
                    remote_id: 0,
                    id,
                });

                let (mut error_reader, error_writer) = os_pipe::pipe()?;
                cmd.stderr(error_writer.into_stdio());

                let (mut output_reader, output_writer) = os_pipe::pipe()?;
                
                let mut do_forward = true;
                if let Some(handle) = self.open_files.remove(&stdout_pipe) {
                    cmd.stdout(handle);
                    do_forward = false;
                } else {
                    cmd.stdout(output_writer.into_stdio());
                }

                let handle = Arc::new(Mutex::new(cmd.spawn()?));
                let cancel_handle = handle.clone();

                drop(cmd);

                let (cancel_send, cancel_recv) = mpsc::channel();

                let is_running = Arc::new(AtomicBool::new(true));

                thread::spawn(move || {
                    cancel_recv.recv().unwrap();
                    eprintln!("{:?} received cancel", id);

                    handle.lock().unwrap().kill().unwrap();

                    eprintln!("{:?} finished cancel", id);
                });

                let stdout_thread = if do_forward {
                    let backtraffic = self.backtraffic.clone();
                    let is_running_clone = is_running.clone();
                    let t = thread::spawn(move || {
                        let mut buf = [0u8; 1024];

                        loop {
                            let len = output_reader.read(&mut buf).unwrap();
                            eprintln!("{:?} read stdout {:?}", id, len);
                            if len == 0 {
                                // I wonder if this could become an infinite loop... :(
                                if !is_running_clone.load(Ordering::SeqCst) {
                                    break;
                                // } else {
                                //     assert!(false, "stdout continued!")
                                }
                            } else {
                                backtraffic.lock().unwrap().pipe(stdout_pipe, buf[..len].to_vec()).unwrap();
                            }
                        }
                    });
                    Some(t)
                } else {
                    None
                };

                let is_running_clone = is_running.clone();
                let backtraffic = self.backtraffic.clone();
                let stderr_thread = thread::spawn(move || {
                    let mut buf = [0u8; 1024];

                    loop {
                        let len = error_reader.read(&mut buf).unwrap();
                        eprintln!("{:?} read stderr {:?}", id, len);
                        if len == 0 {
                            if !is_running_clone.load(Ordering::SeqCst) {
                                // I wonder if this could become an infinite loop... :(
                                break;
                            // } else {
                            //     assert!(false, "stderr continued!")
                            }
                        } else {
                            backtraffic.lock().unwrap().pipe(stderr_pipe, buf[..len].to_vec()).unwrap();
                        }
                    }
                });

                let is_running_clone = is_running.clone();
                let backtraffic = self.backtraffic.clone();
                let running_commands = self.running_commands.clone();
                thread::spawn(move || {
                    let exit_code = cancel_handle.lock().unwrap().wait().unwrap().code().unwrap_or(-1);
                    eprintln!("{:?} exit {}", id, exit_code);
                    is_running_clone.store(false, Ordering::SeqCst);

                    if let Some(t) = stdout_thread {
                        t.join().unwrap();
                    }

                    stderr_thread.join().unwrap();

                    running_commands.lock().unwrap().remove(&command_key);
                    backtraffic.lock().unwrap().command_done(id, exit_code.into()).unwrap();
                });

                Ok(RunResult::Process(ProcessState::Running {
                    cancel: cancel_send,
                }))
            }
            Command::SetDirectory(dir) => {
                let mut back = self.backtraffic.lock().unwrap();
                match env::set_current_dir(dir) {
                    Ok(_) => {
                        back.command_done(id, 1)?;
                    }
                    Err(e) => {
                        back.pipe(stderr_pipe, format!("Error: {:?}", e).into_bytes())?;
                        back.command_done(id, 1)?;
                    }
                }
                Ok(RunResult::AlreadyDone(ExitStatus::Success))
            }
            Command::Edit(path) => {
                let (sender, receiver) = mpsc::channel();

                self.waiting_edits.lock().unwrap().insert(0, sender);


                let mut contents = Vec::new();
                match File::open(&path) {
                    Ok(mut f) => {
                        f.read_to_end(&mut contents)?;
                    }
                    Err(e) => {
                        if e.kind() != io::ErrorKind::NotFound {
                            return Err(e.into());
                        }
                    }
                }

                self.backtraffic.lock().unwrap().edit_request(id, 0, path.clone(), contents).unwrap();

                let backtraffic = self.backtraffic.clone();
                thread::spawn(move || {
                    let resp = receiver.recv().unwrap();

                    File::create(path).unwrap().write_all(&resp).unwrap();

                    backtraffic.lock().unwrap().command_done(id, 0).unwrap();
                });

                Ok(RunResult::Process(ProcessState::AwaitingEdit))
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

    fn cancel(&mut self, id: ProcessId) -> Result<(), Error> {
        match self.machine.status(id) {
            Status::Running(state) => {
                match state {
                    ProcessState::Running { cancel } => {
                        cancel.send(()).unwrap();
                    }
                    _ => panic!(),
                }
            }
            Status::Exited(_) => {
                // TODO: send back an "already exited" or something
            }
        }
        Ok(())
    }

    fn open_file(&mut self, id: WritePipe, path: String) -> Result<(), Error> {
        self.open_files.insert(id, File::create(path)?);
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
        self.enqueue(process.id, cmd, block_for)?;
        Ok(())
    }

    fn cancel_command(&mut self, id: ProcessId) -> Result<(), Error> {
        self.cancel(id)
    }

    fn begin_remote(&mut self, id: usize, command: Command) -> Result<(), Error> {
        self.begin_remote(id, command)
    }

    fn open_file(&mut self, id: WritePipe, path: String) -> Result<(), Error> {
        self.open_file(id, path)
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

    fn finish_edit(&mut self, id: usize, data: Vec<u8>) -> Result<(), Error> {
        self.waiting_edits.lock().unwrap().remove(&id).unwrap().send(data)?;
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

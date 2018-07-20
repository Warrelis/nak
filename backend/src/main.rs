#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate failure;
extern crate os_pipe;

#[cfg(unix)]
extern crate ctrlc;

#[cfg(unix)]
extern crate unix_socket;

extern crate base64;
extern crate rand;
extern crate protocol;

extern crate futures;
extern crate tokio;
extern crate nix;

use std::collections::HashMap;
use std::io::{Write, Read, BufRead, BufReader, Seek, SeekFrom};
use std::{io, env, fs, thread};
use std::fs::{File, OpenOptions};
use std::sync::mpsc;
use std::sync::Mutex;
use std::sync::Arc;
use std::sync::atomic::{Ordering, AtomicBool};
use std::os::unix::prelude::*;

use failure::Error;
use std::process as pr;
use os_pipe::IntoStdio;
use os_pipe::PipeWriter;
// use tokio::prelude::*;
use futures::{Poll, Async};
// use tokio::io::AsyncRead;

#[cfg(unix)]
use unix_socket::{UnixStream, UnixListener};

use rand::{Rng, thread_rng};

use protocol::{Multiplex, RpcResponse, RpcRequest, Command, Process, BackTraffic, Transport};

fn write_edit_file_request(id: usize, edit_id: usize, name: String, data: Vec<u8>) -> Result<(), Error> {
    write!(io::stdout(), "{}\n", serde_json::to_string(&Multiplex {
        remote_id: 0,
        message: RpcResponse::EditRequest {
            command_id: id,
            edit_id,
            name,
            data,
        },
    })?)?;
    io::stdout().flush().unwrap();
    Ok(())
}

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
    id: usize,
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

#[derive(Default)]
pub struct Backend {
    backtraffic: Arc<Mutex<BackTraffic<StdoutTransport>>>,
    running_commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
    waiting_edits: Arc<Mutex<HashMap<usize, mpsc::Sender<Vec<u8>>>>>,
    subbackends: HashMap<usize, BackendRemote>,
    jobs: HashMap<usize, mpsc::Sender<()>>,
    open_files: HashMap<usize, File>,
}

impl Backend {
    fn run(&mut self, id: usize, stdout_pipe: usize, stderr_pipe: usize, c: Command) -> Result<(), Error> {
        eprintln!("{} running {:?}", id, c);
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

                self.jobs.insert(id, cancel_send);
                let running_commands = self.running_commands.clone();

                let is_running = Arc::new(AtomicBool::new(true));


                thread::spawn(move || {
                    cancel_recv.recv().unwrap();
                    eprintln!("{} received cancel", id);

                    handle.lock().unwrap().kill().unwrap();

                    eprintln!("{} finished cancel", id);
                });

                let stdout_thread = if do_forward {
                    let backtraffic = self.backtraffic.clone();
                    let is_running_clone = is_running.clone();
                    let t = thread::spawn(move || {
                        let mut buf = [0u8; 1024];

                        loop {
                            let len = output_reader.read(&mut buf).unwrap();
                            eprintln!("{} read stdout {:?}", id, len);
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
                        eprintln!("{} read stderr {:?}", id, len);
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
                thread::spawn(move || {
                    let exit_code = cancel_handle.lock().unwrap().wait().unwrap().code().unwrap_or(-1);
                    eprintln!("{} exit {}", id, exit_code);
                    is_running_clone.store(false, Ordering::SeqCst);

                    if let Some(t) = stdout_thread {
                        t.join().unwrap();
                    }

                    stderr_thread.join().unwrap();

                    running_commands.lock().unwrap().remove(&command_key);

                    backtraffic.lock().unwrap().command_done(id, exit_code.into()).unwrap();
                });

                Ok(())
            }
            Command::SetDirectory(dir) => {
                let mut back = self.backtraffic.lock().unwrap();
                match env::set_current_dir(dir) {
                    Ok(_) => back.command_done(id, 1),
                    Err(e) => {
                        back.pipe(stderr_pipe, format!("Error: {:?}", e).into_bytes())?;
                        back.command_done(id, 1)
                    }
                }
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

                Ok(())
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

    fn cancel(&mut self, id: usize) -> Result<(), Error> {
        self.jobs.get(&id).unwrap().send(())?;
        Ok(())
    }

    fn open_file(&mut self, id: usize, path: String) -> Result<(), Error> {
        self.open_files.insert(id, File::create(path)?);
        Ok(())
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
    backtraffic: Arc<Mutex<BackTraffic<StdoutTransport>>>,
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
    running_commands: Arc<Mutex<HashMap<String, CommandInfo>>>,
    waiting_edits: Arc<Mutex<HashMap<usize, mpsc::Sender<Vec<u8>>>>>) -> Result<(), Error>
{
    Ok(())
}

fn run_backend() -> Result<(), Error> {

    let mut backend = Backend::default();

    // ctrlc::set_handler(move || {
    //     eprintln!("backend caught CtrlC");
    // }).expect("Error setting CtrlC handler");

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

    loop {
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(n) => {
                if n == 0 {
                    break;
                }
                // eprintln!("part {}", input);

                let rpc: Multiplex<RpcRequest> = serde_json::from_str(&input)?;

                if rpc.remote_id == 0 {
                    match rpc.message {
                        RpcRequest::BeginCommand { process: Process { id, stdout_pipe, stderr_pipe } , command } => {
                            backend.run(id, stdout_pipe, stderr_pipe, command)?;
                        }
                        RpcRequest::BeginRemote { id, command } => {
                            backend.begin_remote(id, command)?;
                        }
                        RpcRequest::OpenFile { id, path } => {
                            backend.open_file(id, path)?;
                        }
                        RpcRequest::EndRemote { id } => {
                            backend.end_remote(id)?;
                        }
                        RpcRequest::CancelCommand { id } => {
                            // eprintln!("caught CtrlC");
                            backend.cancel(id)?;
                        }
                        RpcRequest::ListDirectory { id, path } => {
                            let items = fs::read_dir(path).unwrap().map(|i| {
                                i.unwrap().file_name().into_string().unwrap()
                            }).collect();


                            backend.backtraffic.lock().unwrap().directory_listing(id, items)?;
                        }
                        RpcRequest::FinishEdit { id, data } => {
                            backend.waiting_edits.lock().unwrap().remove(&id).unwrap().send(data)?;
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

struct CommandOutput {
    handle: pr::Child,
    stdout: MyPipeReader,
    stderr: MyPipeReader,
}

enum Status {
    Out(Vec<u8>),
    Err(Vec<u8>),
    Done(i32),
}

struct MyPipeReader(i32);
struct MyPipeWriter(i32);

impl MyPipeWriter {
    fn into_stdio(self) -> pr::Stdio {
        let fd = self.0;
        unsafe { pr::Stdio::from_raw_fd(fd) }
    }
}

impl Read for MyPipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        nix::unistd::read(self.0, buf).map_err(|_| panic!())
    }
}

fn nix_err_to_io_err(err: nix::Error) -> io::Error {
    if let nix::Error::Sys(err_no) = err {
        io::Error::from(err_no)
    } else {
        panic!("unexpected nix error type: {:?}", err)
    }
}


impl MyPipeReader {
    fn poll_read(&mut self, buf: &mut [u8]) -> Poll<usize, io::Error> {
        match nix::unistd::read(self.0, buf) {
            Ok(len) => Ok(Async::Ready(len)),
            Err(e) => {
                let e = nix_err_to_io_err(e);
                if e.kind() == io::ErrorKind::WouldBlock {
                    Ok(Async::Pending)
                } else {
                    Err(e.into())
                }
            }
        }
    }
}

fn my_pipe() -> Result<(MyPipeReader, MyPipeWriter), Error> {
    let (read_fd, write_fd) = nix::unistd::pipe()?;
    nix::fcntl::fcntl(read_fd, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_CLOEXEC))?;
    nix::fcntl::fcntl(write_fd, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_CLOEXEC))?;
    nix::fcntl::fcntl(read_fd, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK))?;

    Ok((
        MyPipeReader(read_fd),
        MyPipeWriter(write_fd),
    ))
}

impl CommandOutput {
    fn next_status(&mut self) -> Poll<Status, Error> {
        panic!();
    }
}

fn start_py() -> Result<CommandOutput, Error> {
    let mut cmd = pr::Command::new("python3");
    cmd.args(&["-i"]);

    let (error_reader, error_writer) = my_pipe()?;
    cmd.stderr(error_writer.into_stdio());

    let (output_reader, output_writer) = my_pipe()?;
    cmd.stdout(output_writer.into_stdio());

    let handle = cmd.spawn()?;

    drop(cmd);

    Ok(CommandOutput {
        handle,
        stdout: output_reader,
        stderr: error_reader,
    })
}

fn run_experiment() -> Result<(), Error> {
    let mut cmd = start_py()?;

    let mut reactor = tokio::reactor::Reactor::new();

    let mut buf = [0u8; 1024];

    loop {
        match cmd.stdout.poll_read(&mut buf) {
            Ok(Async::Pending) => {

            }
            Ok(Async::Ready(len)) => {
                eprintln!("got out {}!", len);
            }
            Err(e) => {
                panic!(e);
            }
        }

        match cmd.stderr.poll_read(&mut buf) {
            Ok(Async::Pending) => {

            }
            Ok(Async::Ready(len)) => {
                eprintln!("got err {}!", len);
            }
            Err(e) => {
                panic!(e);
            }
        }

        match cmd.handle.try_wait() {
            Ok(Some(status)) => {
                eprintln!("exited with {}", status.code().unwrap_or(-1));
                break;
            }
            Ok(None) => {}
            Err(e) => {
                panic!(e);
            }
        }
    }

    // loop {
    //     let len = c.stdout.read(&mut buf).unwrap();
    //     // eprintln!("{} read stdout {:?}", id, len);
    //     if len == 0 {
    //         break;
    //     }

    //     io::stdout().write(&buf[..len])?;
    // }

    // loop {
    //     let len = c.stderr.read(&mut buf).unwrap();
    //     // eprintln!("{} read stderr {:?}", id, len);
    //     if len == 0 {
    //         break;
    //     }

    //     io::stderr().write(&buf[..len])?;
    // }

    let exit_code = cmd.handle.wait().unwrap().code().unwrap_or(-1);
    eprintln!("exit {}", exit_code);

    Ok(())
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

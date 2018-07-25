use std::fs::File;
use std::sync::mpsc;
use std::collections::HashMap;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::process as pr;
use std::io;
use std::io::{Read};
use std::sync::{Arc, Mutex};
use std::env;

use failure::Error;
use os_pipe;
use os_pipe::{IntoStdio};

use protocol::{ProcessId, Command, ReadPipe, WritePipe, ExitStatus, Condition};

use machine::{Machine, Task, Status};

pub struct RunCmd {
    pub cmd: Command,
    pub stdin: ReadPipe,
    pub stdout: WritePipe,
    pub stderr: WritePipe,
}

enum ProcessState {
    Running {
        handle: Arc<Mutex<pr::Child>>,
    },
    AwaitingEdit,
}

enum RunResult {
    Process(ProcessState),
    AlreadyDone(i64),
}

enum ExecEvent {
    Enqueue(ProcessId, RunCmd, HashMap<ProcessId, Condition>),
    Completed(ProcessId, i64),
    OpenOutputFile(WritePipe, String),
    CancelExec(ProcessId),
    PipeOutput(WritePipe, Vec<u8>),
}

struct ExecInternal {
    handler: Box<dyn Handler>,
    sender: mpsc::Sender<ExecEvent>,
    receiver: mpsc::Receiver<ExecEvent>,
    machine: Machine<ProcessId, RunCmd, ProcessState>,
    open_output_files: HashMap<WritePipe, File>,
}

impl ExecInternal {
    fn run_handler(&mut self) {
        loop {
            let cmd = match self.receiver.recv() {
                Ok(cmd) => cmd,
                Err(e) => panic!("{:#?}", e),
            };

            match cmd {
                ExecEvent::Enqueue(pid, cmd, block_for) => {
                    self.enqueue(pid, cmd, block_for).unwrap();
                }
                ExecEvent::Completed(pid, exit_status) => {
                    self.completed(pid, exit_status).unwrap();
                }
                ExecEvent::OpenOutputFile(pipe, path) => {
                    self.open_output_file(pipe, path).unwrap();
                }
                ExecEvent::CancelExec(pid) => {
                    self.cancel(pid).unwrap();
                }
                ExecEvent::PipeOutput(pipe, data) => {
                    self.pipe_output(pipe, data).unwrap();
                }
            }
        }
    }


    fn process_tasks(&mut self, mut tasks: Vec<Task<ProcessId, RunCmd>>) -> Result<(), Error> {
        loop {
            let mut new_tasks = Vec::new();

            for t in tasks {
                match t {
                    Task::Start(pid, cmd) => {
                        match self.run(pid, cmd.stdout, cmd.stderr, cmd.cmd) {
                            Ok(RunResult::Process(state)) => self.machine.start(pid, state),
                            Ok(RunResult::AlreadyDone(exit_code)) => {
                                new_tasks.extend(
                                    self.machine.start_completed(pid, ExitStatus::from_exit_code(exit_code)));
                                self.handler.command_result(pid, 0).unwrap();
                            }
                            Err(e) => {
                                new_tasks.extend(
                                    self.machine.start_completed(pid, ExitStatus::Failure));
                                self.pipe_output(cmd.stderr, format!("{:?}", e).into_bytes())?;
                                self.handler.command_result(pid, 1)?;
                            }
                        }
                    }
                    Task::ConditionFailed(pid, _) => {
                        self.handler.command_result(pid, 1).unwrap();
                    }
                }
            }

            if new_tasks.len() == 0 {
                break;
            }

            tasks = new_tasks;
        }

        Ok(())
    }

    fn enqueue(&mut self, pid: ProcessId, cmd: RunCmd, block_for: HashMap<ProcessId, Condition>) -> Result<(), Error> {
        let tasks = self.machine.enqueue(pid, cmd, block_for);
        self.process_tasks(tasks)
    }

    fn completed(&mut self, pid: ProcessId, exit_code: i64) -> Result<(), Error> {
        self.handler.command_result(pid, exit_code).unwrap();
        let tasks = self.machine.completed(pid, ExitStatus::from_exit_code(exit_code));
        self.process_tasks(tasks)
    }

    fn run(&mut self, pid: ProcessId, stdout_pipe: WritePipe, stderr_pipe: WritePipe, c: Command) -> Result<RunResult, Error> {
        eprintln!("{:?} running {:?}", pid, c);
        match c {
            Command::Unknown(path, args) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&args);

                // let command_key = random_key();
                // cmd.env("NAK_COMMAND_KEY", &command_key);
                // self.running_commands.lock().unwrap().insert(command_key.clone(), CommandInfo {
                //     remote_id: 0,
                //     id,
                // });

                let (mut error_reader, error_writer) = os_pipe::pipe()?;
                cmd.stderr(error_writer.into_stdio());

                let (mut output_reader, output_writer) = os_pipe::pipe()?;
                
                let mut do_forward = true;
                if let Some(handle) = self.open_output_files.remove(&stdout_pipe) {
                    cmd.stdout(handle);
                    do_forward = false;
                } else {
                    cmd.stdout(output_writer.into_stdio());
                }

                let handle = Arc::new(Mutex::new(cmd.spawn()?));
                let cancel_handle = handle.clone();

                drop(cmd);

                let is_running = Arc::new(AtomicBool::new(true));

                let stdout_thread = if do_forward {
                    let sender = self.sender.clone();
                    let is_running_clone = is_running.clone();
                    let t = thread::spawn(move || {
                        let mut buf = [0u8; 1024];

                        loop {
                            let len = output_reader.read(&mut buf).unwrap();
                            eprintln!("{:?} read stdout {:?}", pid, len);
                            if len == 0 {
                                // I wonder if this could become an infinite loop... :(
                                if !is_running_clone.load(Ordering::SeqCst) {
                                    break;
                                // } else {
                                //     assert!(false, "stdout continued!")
                                }
                            } else {
                                sender.send(ExecEvent::PipeOutput(stdout_pipe, buf[..len].to_vec())).unwrap();
                            }
                        }
                    });
                    Some(t)
                } else {
                    None
                };

                let is_running_clone = is_running.clone();
                let sender = self.sender.clone();
                let stderr_thread = thread::spawn(move || {
                    let mut buf = [0u8; 1024];

                    loop {
                        let len = error_reader.read(&mut buf).unwrap();
                        eprintln!("{:?} read stderr {:?}", pid, len);
                        if len == 0 {
                            if !is_running_clone.load(Ordering::SeqCst) {
                                // I wonder if this could become an infinite loop... :(
                                break;
                            // } else {
                            //     assert!(false, "stderr continued!")
                            }
                        } else {
                            sender.send(ExecEvent::PipeOutput(stderr_pipe, buf[..len].to_vec())).unwrap();
                        }
                    }
                });

                let sender = self.sender.clone();
                let is_running_clone = is_running.clone();
                // let backtraffic = self.backtraffic.clone();
                // let running_commands = self.running_commands.clone();
                thread::spawn(move || {
                    let exit_code = cancel_handle.lock().unwrap().wait().unwrap().code().unwrap_or(-1);
                    eprintln!("{:?} exit {}", pid, exit_code);
                    sender.send(ExecEvent::Completed(pid, exit_code.into())).unwrap();
                    is_running_clone.store(false, Ordering::SeqCst);

                    if let Some(t) = stdout_thread {
                        t.join().unwrap();
                    }

                    stderr_thread.join().unwrap();

                    // running_commands.lock().unwrap().remove(&command_key);
                });

                Ok(RunResult::Process(ProcessState::Running {
                    handle,
                }))
            }
            Command::SetDirectory(dir) => {
                match env::set_current_dir(dir) {
                    Ok(o) => {
                        self.pipe_output(stderr_pipe, format!("Success: {:?}", o).into_bytes())?;
                        Ok(RunResult::AlreadyDone(0))
                    }
                    Err(e) => {
                        self.pipe_output(stderr_pipe, format!("Error: {:?}", e).into_bytes())?;
                        Ok(RunResult::AlreadyDone(1))
                    }
                }
            }
            Command::GetDirectory => {
                match env::current_dir() {
                    Ok(dir) => {
                        self.pipe_output(stdout_pipe, format!("{}\n", dir.display()).into_bytes())?;
                        Ok(RunResult::AlreadyDone(0))
                    }
                    Err(e) => {
                        self.pipe_output(stderr_pipe, format!("Error: {:?}", e).into_bytes())?;
                        Ok(RunResult::AlreadyDone(1))
                    }
                }
            }
            Command::Edit(path) => {
                // let (sender, receiver) = mpsc::channel();

                // self.waiting_edits.lock().unwrap().insert(0, sender);


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

                // self.backtraffic.lock().unwrap().edit_request(id, 0, path.clone(), contents).unwrap();

                // let backtraffic = self.backtraffic.clone();
                // thread::spawn(move || {
                //     let resp = receiver.recv().unwrap();

                //     File::create(path).unwrap().write_all(&resp).unwrap();

                //     backtraffic.lock().unwrap().command_done(id, 0).unwrap();
                // });

                Ok(RunResult::Process(ProcessState::AwaitingEdit))
            }
        }
    }

    fn open_output_file(&mut self, pipe: WritePipe, path: String) -> Result<(), Error> {
        self.open_output_files.insert(pipe, File::create(path)?);
        Ok(())
    }

    fn pipe_output(&mut self, pipe: WritePipe, data: Vec<u8>) -> Result<(), Error> {
        self.handler.pipe_output(pipe, data)
    }

    fn cancel(&mut self, pid: ProcessId) -> Result<(), Error> {
        match self.machine.status(pid) {
            Status::Running(state) => {
                match state {
                    ProcessState::Running { handle } => {
                        handle.lock().unwrap().kill()?;
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
}

pub trait Handler: Send + 'static {
    fn pipe_output(&mut self, pipe: WritePipe, data: Vec<u8>) -> Result<(), Error>;
    fn command_result(&mut self, pid: ProcessId, exit_code: i64) -> Result<(), Error>;
}

pub struct Exec {
    thread: thread::JoinHandle<()>,
    sender: mpsc::Sender<ExecEvent>,
}

impl Exec {
    pub fn new(handler: Box<dyn Handler>) -> Exec {
        let (sender, receiver) = mpsc::channel();

        let mut intern = ExecInternal {
            sender: sender.clone(),
            receiver,
            machine: Machine::new(),
            open_output_files: HashMap::new(),
            handler,
        };

        let t = thread::spawn(move || intern.run_handler());

        Exec {
            thread: t,
            sender,
        }
    }

    pub fn enqueue(&self, pid: ProcessId, cmd: RunCmd, block_for: HashMap<ProcessId, Condition>) -> Result<(), Error> {
        self.sender.send(ExecEvent::Enqueue(pid, cmd, block_for)).unwrap();
        Ok(())
    }

    pub fn cancel(&self, pid: ProcessId) -> Result<(), Error> {
        self.sender.send(ExecEvent::CancelExec(pid)).unwrap();
        Ok(())
    }

    pub fn open_output_file(&self, pipe: WritePipe, path: String) -> Result<(), Error> {
        self.sender.send(ExecEvent::OpenOutputFile(pipe, path)).unwrap();
        Ok(())
    }
}


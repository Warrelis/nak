use std::fs::File;
use std::sync::mpsc;
use std::collections::HashMap;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::process as pr;
use std::io;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::env;

use failure::Error;
use os_pipe;
use os_pipe::{IntoStdio};

use protocol::{ProcessId, Command, ReadPipe, WritePipe, ExitStatus, Condition, Ids, GenericPipe};

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

enum OutputPipe {
    File(File),
    Pipe(os_pipe::PipeWriter),
}

enum InputPipe {
    File(File),
    Pipe(os_pipe::PipeReader),
}

enum ExecEvent {
    Enqueue(ProcessId, RunCmd, HashMap<ProcessId, Condition>),
    Completed(ProcessId, i64),
    OpenOutputFile(WritePipe, String),
    OpenInputFile(ReadPipe, String),
    CancelExec(ProcessId),
    PipeOutput(WritePipe, Vec<u8>),
    EditComplete(usize, Vec<u8>),
}

struct ExecInternal {
    edit_ids: Ids,
    handler: Box<dyn Handler>,
    sender: mpsc::Sender<ExecEvent>,
    receiver: mpsc::Receiver<ExecEvent>,
    machine: Machine<ProcessId, RunCmd, ProcessState>,
    open_output_handles: HashMap<GenericPipe, OutputPipe>,
    open_input_handles: HashMap<GenericPipe, InputPipe>,
    waiting_edits: HashMap<usize, (ProcessId, String)>,
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
                ExecEvent::OpenInputFile(pipe, path) => {
                    self.open_input_file(pipe, path).unwrap();
                }
                ExecEvent::CancelExec(pid) => {
                    self.cancel(pid).unwrap();
                }
                ExecEvent::PipeOutput(pipe, data) => {
                    self.pipe_output(pipe, data).unwrap();
                }
                ExecEvent::EditComplete(edit_id, data) => {
                    self.finish_edit(edit_id, data).unwrap();
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
                        let stderr = cmd.stderr;
                        match self.run(pid, cmd) {
                            Ok(RunResult::Process(state)) => self.machine.start(pid, state),
                            Ok(RunResult::AlreadyDone(exit_code)) => {
                                new_tasks.extend(
                                    self.machine.start_completed(pid, ExitStatus::from_exit_code(exit_code)));
                                self.handler.command_result(pid, 0).unwrap();
                            }
                            Err(e) => {
                                new_tasks.extend(
                                    self.machine.start_completed(pid, ExitStatus::Failure));
                                // TODO: perhaps this should go back on a custom error stream (rather than stderr?)
                                self.pipe_output(stderr, format!("{:?}", e).into_bytes())?;
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

    fn run(&mut self, pid: ProcessId, c: RunCmd) -> Result<RunResult, Error> {
        eprintln!("{:?} running {:?}", pid, c.cmd);
        match c.cmd {
            Command::Unknown(path, args) => {
                let mut cmd = pr::Command::new(path);
                cmd.args(&args);

                // let command_key = random_key();
                // cmd.env("NAK_COMMAND_KEY", &command_key);
                // self.running_commands.lock().unwrap().insert(command_key.clone(), CommandInfo {
                //     remote_id: 0,
                //     id,
                // });

                if let Some(handle) = self.open_input_handles.remove(&c.stdin.to_generic()) {
                    match handle {
                        InputPipe::File(f) => cmd.stdin(f),
                        InputPipe::Pipe(f) => cmd.stdin(f.into_stdio()),
                    };
                } else {
                    let (input_reader, input_writer) = os_pipe::pipe()?;
                    cmd.stdin(input_reader.into_stdio());
                    self.open_output_handles.insert(c.stdin.to_generic(), OutputPipe::Pipe(input_writer));
                }

                if let Some(handle) = self.open_output_handles.remove(&c.stdout.to_generic()) {
                    match handle {
                        OutputPipe::File(f) => cmd.stdout(f),
                        OutputPipe::Pipe(p) => cmd.stdout(p.into_stdio()),
                    };
                } else {
                    let (output_reader, output_writer) = os_pipe::pipe()?;
                    cmd.stdout(output_writer.into_stdio());
                    self.open_input_handles.insert(c.stdout.to_generic(), InputPipe::Pipe(output_reader));
                }

                if let Some(handle) = self.open_output_handles.remove(&c.stderr.to_generic()) {
                    match handle {
                        OutputPipe::File(f) => cmd.stdout(f),
                        OutputPipe::Pipe(p) => cmd.stdout(p.into_stdio()),
                    };
                } else {
                    let (error_reader, error_writer) = os_pipe::pipe()?;
                    cmd.stdout(error_writer.into_stdio());
                    self.open_input_handles.insert(c.stderr.to_generic(), InputPipe::Pipe(error_reader));
                }

                let handle = Arc::new(Mutex::new(cmd.spawn()?));
                let cancel_handle = handle.clone();

                drop(cmd);

                let is_running = Arc::new(AtomicBool::new(true));

                let is_running_clone = is_running.clone();
                let sender = self.sender.clone();

                let sender = self.sender.clone();
                let is_running_clone = is_running.clone();
                // let backtraffic = self.backtraffic.clone();
                // let running_commands = self.running_commands.clone();
                thread::spawn(move || {
                    let exit_code = cancel_handle.lock().unwrap().wait().unwrap().code().unwrap_or(-1);
                    eprintln!("{:?} exit {}", pid, exit_code);
                    sender.send(ExecEvent::Completed(pid, exit_code.into())).unwrap();
                    is_running_clone.store(false, Ordering::SeqCst);

                    // running_commands.lock().unwrap().remove(&command_key);
                });

                Ok(RunResult::Process(ProcessState::Running {
                    handle,
                }))
            }
            Command::SetDirectory(dir) => {
                match env::set_current_dir(dir) {
                    Ok(o) => {
                        self.pipe_output(c.stderr, format!("Success: {:?}", o).into_bytes())?;
                        Ok(RunResult::AlreadyDone(0))
                    }
                    Err(e) => {
                        self.pipe_output(c.stderr, format!("Error: {:?}", e).into_bytes())?;
                        Ok(RunResult::AlreadyDone(1))
                    }
                }
            }
            Command::GetDirectory => {
                match env::current_dir() {
                    Ok(dir) => {
                        self.pipe_output(c.stdout, format!("{}\n", dir.display()).into_bytes())?;
                        Ok(RunResult::AlreadyDone(0))
                    }
                    Err(e) => {
                        self.pipe_output(c.stderr, format!("Error: {:?}", e).into_bytes())?;
                        Ok(RunResult::AlreadyDone(1))
                    }
                }
            }
            Command::Edit(path) => {
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

                self.begin_edit_request(pid, path.clone(), contents)?;

                Ok(RunResult::Process(ProcessState::AwaitingEdit))
            }
        }
    }

    fn open_output_file(&mut self, pipe: WritePipe, path: String) -> Result<(), Error> {
        self.open_output_handles.insert(pipe.to_generic(), OutputPipe::File(File::create(path)?));
        Ok(())
    }

    fn open_input_file(&mut self, pipe: ReadPipe, path: String) -> Result<(), Error> {
        self.open_input_handles.insert(pipe.to_generic(), InputPipe::File(File::open(path)?));
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

    fn begin_edit_request(&mut self, pid: ProcessId, path: String, data: Vec<u8>) -> Result<(), Error> {
        let edit_id = self.edit_ids.next();
        self.waiting_edits.insert(edit_id, (pid, path.clone()));
        self.handler.edit_request(pid, edit_id, path, data)?;
        Ok(())
    }

    fn finish_edit(&mut self, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        let (pid, path) = self.waiting_edits.remove(&edit_id).unwrap();
        File::create(path)?.write_all(&data).unwrap();
        self.completed(pid, 0)?;
        Ok(())
    }
}

pub trait Handler: Send + 'static {
    fn pipe_output(&mut self, pipe: WritePipe, data: Vec<u8>) -> Result<(), Error>;
    fn command_result(&mut self, pid: ProcessId, exit_code: i64) -> Result<(), Error>;
    fn edit_request(&mut self, pid: ProcessId, edit_id: usize, name: String, data: Vec<u8>) -> Result<(), Error>;
}

pub struct Exec {
    thread: thread::JoinHandle<()>,
    sender: mpsc::Sender<ExecEvent>,
}

impl Exec {
    pub fn new(handler: Box<dyn Handler>) -> Exec {
        let (sender, receiver) = mpsc::channel();

        let mut intern = ExecInternal {
            edit_ids: Ids::new(),
            sender: sender.clone(),
            receiver,
            machine: Machine::new(),
            open_output_handles: HashMap::new(),
            open_input_handles: HashMap::new(),
            handler,
            waiting_edits: HashMap::new(),
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

    pub fn open_input_file(&self, pipe: ReadPipe, path: String) -> Result<(), Error> {
        self.sender.send(ExecEvent::OpenInputFile(pipe, path)).unwrap();
        Ok(())
    }

    pub fn finish_edit(&self, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        self.sender.send(ExecEvent::EditComplete(edit_id, data)).unwrap();
        Ok(())
    }
}


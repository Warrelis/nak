use std::fs::File;
use std::sync::mpsc;
use std::collections::HashMap;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::os::unix::prelude::*;
use std::process as pr;
use std::io;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::env;

use failure::Error;
use os_pipe;
use os_pipe::{IntoStdio};

use protocol::{
    ProcessId,
    Command,
    ReadPipe,
    WritePipe,
    WritePipes,
    ExitStatus,
    Condition,
    Ids,
    GenericPipe,
    PipeMessage,
};

use machine::{Machine, Task, Status};

pub struct RunCmd {
    pub cmd: Command,
    pub pipes: WritePipes,
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

struct OutputPipe {
    handle: File,
}

impl OutputPipe {
    fn from_pipe(pipe: os_pipe::PipeWriter) -> OutputPipe {
        OutputPipe {
            handle: unsafe { File::from_raw_fd(pipe.into_raw_fd()) },
        }
    }

    fn from_file(file: File) -> OutputPipe {
        OutputPipe {
            handle: file,
        }
    }

    fn assign_stdout(self, cmd: &mut pr::Command) {
        cmd.stdout(self.handle.into_stdio());
    }

    fn assign_stderr(self, cmd: &mut pr::Command) {
        cmd.stderr(self.handle.into_stdio());
    }
}

struct InputPipe {
    handle: File,
}

impl InputPipe {
    fn from_pipe(pipe: os_pipe::PipeReader) -> InputPipe {
        InputPipe {
            handle: unsafe { File::from_raw_fd(pipe.into_raw_fd()) },
        }
    }

    fn from_file(file: File) -> InputPipe {
        InputPipe {
            handle: file,
        }
    }

    fn assign_stdin(self, cmd: &mut pr::Command) {
        cmd.stdin(self.handle.into_stdio());
    }
}

enum ExecEvent {
    Enqueue(ProcessId, RunCmd, HashMap<ProcessId, Condition>),
    Completed(ProcessId, i64),
    OpenOutputFile(WritePipe, String),
    OpenInputFile(ReadPipe, String),
    CancelExec(ProcessId),
    PipeClosed(GenericPipe, u64),
    PipeOutput(GenericPipe, Vec<u8>, u64),
    PipeMessage(GenericPipe, PipeMessage),
    EditComplete(usize, Vec<u8>),
}

struct Pair {
    read: Option<InputPipe>,
    write: Option<OutputPipe>,
}

struct ExecInternal {
    edit_ids: Ids,
    handler: Box<dyn Handler>,
    sender: mpsc::Sender<ExecEvent>,
    receiver: mpsc::Receiver<ExecEvent>,
    machine: Machine<ProcessId, RunCmd, ProcessState>,
    open_handles: HashMap<GenericPipe, Pair>,
    actively_reading: HashMap<GenericPipe, thread::JoinHandle<()>>,
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
                ExecEvent::PipeClosed(pipe, end_offset) => {
                    self.handler.pipe_closed(pipe, end_offset).unwrap();
                }
                ExecEvent::PipeOutput(pipe, data, end_offset) => {
                    self.handler.pipe_output(pipe, data, end_offset).unwrap();
                }
                ExecEvent::PipeMessage(pipe, msg) => {
                    self.pipe_message(pipe, msg).unwrap();
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
                        let pipes = cmd.pipes;
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
                                self.pipe_output_and_close(pipes, vec![], format!("{:?}", e).into_bytes())?;
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

    fn read_end(&mut self, pipe: ReadPipe) -> InputPipe {
        if self.open_handles.contains_key(&pipe.to_generic()) {
            let handle = self.open_handles.get_mut(&pipe.to_generic()).expect("already checked!?!?!");
            handle.read.take().expect("read side taken")
        } else {
            let (r, w) = os_pipe::pipe().expect("alloc pipe failed");
            self.open_handles.insert(pipe.to_generic(), Pair {
                read: None,
                write: Some(OutputPipe::from_pipe(w)),
            });
            InputPipe::from_pipe(r)
        }
    }

    fn write_end(&mut self, pipe: WritePipe) -> OutputPipe {
        if self.open_handles.contains_key(&pipe.to_generic()) {
            let handle = self.open_handles.get_mut(&pipe.to_generic()).expect("already checked!?!?!");
            handle.write.take().expect("write side taken")
        } else {
            let (r, w) = os_pipe::pipe().expect("alloc pipe failed");
            self.open_handles.insert(pipe.to_generic(), Pair {
                read: Some(InputPipe::from_pipe(r)),
                write: None,
            });
            OutputPipe::from_pipe(w)
        }
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

                self.read_end(c.pipes.stdin).assign_stdin(&mut cmd);
                self.write_end(c.pipes.stdout).assign_stdout(&mut cmd);
                self.write_end(c.pipes.stderr).assign_stderr(&mut cmd);

                let handle = Arc::new(Mutex::new(cmd.spawn()?));
                let cancel_handle = handle.clone();

                drop(cmd);

                let is_running = Arc::new(AtomicBool::new(true));

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
                        self.pipe_output_and_close(c.pipes,
                            format!("Success: {:?}", o).into_bytes(),
                            vec![])?;
                        Ok(RunResult::AlreadyDone(0))
                    }
                    Err(e) => {
                        self.pipe_output_and_close(c.pipes,
                            format!("Error: {:?}", e).into_bytes(),
                            vec![])?;
                        Ok(RunResult::AlreadyDone(1))
                    }
                }
            }
            Command::GetDirectory => {
                match env::current_dir() {
                    Ok(dir) => {
                        self.pipe_output_and_close(c.pipes, format!("{}\n", dir.display()).into_bytes(), vec![])?;
                        Ok(RunResult::AlreadyDone(0))
                    }
                    Err(e) => {
                        self.pipe_output_and_close(c.pipes, vec![], format!("Error: {:?}", e).into_bytes())?;
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
        assert!(!self.open_handles.contains_key(&pipe.to_generic()));
        self.open_handles.insert(pipe.to_generic(), Pair {
            read: None,
            write: Some(OutputPipe::from_file(File::create(path)?)),
        });
        Ok(())
    }

    fn open_input_file(&mut self, pipe: ReadPipe, path: String) -> Result<(), Error> {
        assert!(!self.open_handles.contains_key(&pipe.to_generic()));
        self.open_handles.insert(pipe.to_generic(), Pair {
            read: Some(InputPipe::from_file(File::open(path)?)),
            write: None,
        });
        Ok(())
    }

    fn pipe_message(&mut self, pipe: GenericPipe, msg: PipeMessage) -> Result<(), Error> {
        eprintln!("got pipe message {:?} {:?}", pipe, msg);
        match msg {
            PipeMessage::BeginRead => {
                let mut handle = self.read_end(pipe.to_read());
                let sender = self.sender.clone();
                let reader = thread::spawn(move || {
                    let mut buf = [0u8; 1024];
                    loop {
                        match handle.handle.read(&mut buf) {
                            Ok(0) => break,
                            Ok(len) => {
                                eprintln!("read {:?} {:?}", pipe, len);
                                sender.send(ExecEvent::PipeOutput(
                                    pipe,
                                    buf[..len].to_owned(),
                                    0 // TODO!!!!!!
                                )).expect("sending file read");
                            }
                            Err(e) => {
                                panic!("{:?}", e);
                            }
                        }
                    }
                    eprintln!("eof {:?}", pipe);
                    sender.send(ExecEvent::PipeClosed(
                        pipe,
                        0 // TODO!!!!!
                    )).expect("sending buffer read");
                });

                self.actively_reading.insert(pipe, reader);
            }
            PipeMessage::Read { read_up_to: _ } => {
                // we don't support limiting reads at this point.  The data just keeps flowing.
            }
            PipeMessage::Closed { .. } |
            PipeMessage::Data { .. } => panic!()
        }
        Ok(())
    }

    fn pipe_output_and_close(&mut self, pipes: WritePipes, stdout: Vec<u8>, stderr: Vec<u8>) -> Result<(), Error> {
        fn pipe_and_close(pipe: WritePipe, mut handle: OutputPipe, data: Vec<u8>) -> Result<(), Error> {

            if data.len() > 0 {
                thread::spawn(move || {
                    handle.handle.write_all(&data).expect("write all input buffer");
                    eprintln!("closing {:?} {:?}", pipe, data.len());
                });
            } else {
                drop(handle);
            }
            Ok(())
        }

        pipe_and_close(pipes.stdout, self.write_end(pipes.stdout), stdout)?;
        pipe_and_close(pipes.stderr, self.write_end(pipes.stderr), stderr)?;
        Ok(())
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
    fn pipe_output(&mut self, pipe: GenericPipe, data: Vec<u8>, end_offset: u64) -> Result<(), Error>;
    fn pipe_closed(&mut self, pipe: GenericPipe, end_offset: u64) -> Result<(), Error>;
    fn command_result(&mut self, pid: ProcessId, exit_code: i64) -> Result<(), Error>;
    fn edit_request(&mut self, pid: ProcessId, edit_id: usize, name: String, data: Vec<u8>) -> Result<(), Error>;
}

pub struct Exec {
    // thread: thread::JoinHandle<()>,
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
            open_handles: HashMap::new(),
            actively_reading: HashMap::new(),
            handler,
            waiting_edits: HashMap::new(),
        };

        thread::spawn(move || intern.run_handler());

        Exec {
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

    pub fn pipe(&self, id: GenericPipe, msg: PipeMessage) -> Result<(), Error> {
        self.sender.send(ExecEvent::PipeMessage(id, msg)).unwrap();
        Ok(())
    }

    pub fn finish_edit(&self, edit_id: usize, data: Vec<u8>) -> Result<(), Error> {
        self.sender.send(ExecEvent::EditComplete(edit_id, data)).unwrap();
        Ok(())
    }
}


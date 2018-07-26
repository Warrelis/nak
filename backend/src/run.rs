use std::io;
use std::io::{Read};
use failure::Error;
use std::process as pr;
use futures::{Poll, Async};

use std::os::unix::prelude::*;
use nix;
use tokio;

struct CommandOutput {
    handle: pr::Child,
    stdout: MyPipeReader,
    stderr: MyPipeReader,
}

// enum Status {
//     Out(Vec<u8>),
//     Err(Vec<u8>),
//     Done(i32),
// }

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

// impl CommandOutput {
//     fn next_status(&mut self) -> Poll<Status, Error> {
//         panic!();
//     }
// }

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

pub fn run_experiment() -> Result<(), Error> {
    let mut cmd = start_py()?;

    // let mut reactor = tokio::reactor::Reactor::new();

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
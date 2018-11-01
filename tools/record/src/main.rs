
use std::io::Stdin;
use std::os::unix::io::RawFd;
use std::ffi::OsStr;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process as pr;
use std::{io, fmt, str};
use std::time::{Instant, Duration};
use std::env;

use failure::Error;
use nix::pty::openpty;
use nix::pty::Winsize;
use nix::sys::termios::{tcgetattr, Termios, SetArg, InputFlags, LocalFlags};
use nix::sys::termios::tcsetattr;
use nix::libc::TIOCSCTTY;
use nix::libc::ioctl;
use nix::sys::select::{select, FdSet};
use termion::color;
use vte;

struct ColorCoded<'a>(&'a [u8]);

impl<'a> fmt::Display for ColorCoded<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut i = 0;
        while i < self.0.len() {
            if self.0[i] >= 0x20 && self.0[i] <= 0x7e {
                let begin = i;
                while i < self.0.len() && self.0[i] >= 0x20 && self.0[i] <= 0x7e {
                    i += 1;
                }
                write!(f, "{}", str::from_utf8(&self.0[begin..i]).unwrap())?;
            } else {
                write!(f, "{}", color::Fg(color::Red))?;

                while i < self.0.len() && (self.0[i] < 0x20 || self.0[i] > 0x7e) {
                    write!(f, "{:02x}", self.0[i]);
                    i += 1;
                }

                write!(f, "{}", color::Fg(color::Reset))?;
            }
        }

        Ok(())
    }
}

struct PrintPerformer;

impl vte::Perform for PrintPerformer {
    fn print(&mut self, ch: char) {
        // eprintln!("print(ch: {:?})", ch);
    }

    fn execute(&mut self, byte: u8) {
        // eprintln!("execute(byte: {:x?})", byte);
    }

    fn hook(&mut self, params: &[i64], intermediates: &[u8], ignore: bool) {
        // eprintln!("hook(params: {:?}, intermediates: {:?}, ignore: {:?})", params, intermediates, ignore);
    }

    fn put(&mut self, byte: u8) {
        // eprintln!("put(byte: {:x?})", byte);
    }

    fn unhook(&mut self) {
        // eprintln!("unhook()");
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        // eprintln!("osc_dispatch(params: {:?})", params);
    }

    fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], ignore: bool, ch: char) {
        // eprintln!("csi_dispatch(params: {:?}, intermediates: {:?}, ignore: {:?}, ch: {:?})",
            // params, intermediates, ignore, ch);
    }


    fn esc_dispatch(&mut self, params: &[i64], intermediates: &[u8], ignore: bool, byte: u8) {
        // eprintln!("esc_dispatch(params: {:?}, intermediates: {:?}, ignore: {:?}, byte: {:x?})",
            // params,
            // intermediates,
            // ignore,
            // byte);
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Attrs {
    input_flags: u64,
    output_flags: u64,
    control_flags: u64,
    local_flags: u64,
    control_chars: Vec<u8>,
}

impl From<Termios> for Attrs {
    fn from(t: Termios) -> Attrs {
        Attrs {
            input_flags: t.input_flags.bits(),
            output_flags: t.output_flags.bits(),
            control_flags: t.control_flags.bits(),
            local_flags: t.local_flags.bits(),
            control_chars: t.control_chars.to_vec(),
        }
    }
}

#[derive(Clone)]
enum Event {
    Input(Vec<u8>),
    Output(Vec<u8>),
    Config(Attrs),
}

#[derive(Clone)]
struct Record {
    ts: Duration,
    event: Event,
}

#[derive(Clone)]
struct Recording {
    records: Vec<Record>,
}

impl fmt::Display for Recording {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for rec in &self.records {
            let (name, data): (&str, Box<fmt::Display>) = match &rec.event {
                Event::Input(data) => ("In", Box::new(ColorCoded(&data))),
                Event::Output(data) => ("Out", Box::new(ColorCoded(&data))),
                Event::Config(config) => ("Config", Box::new(format!("{:?}", config))),
            };
            let ts = rec.ts.as_secs() as f64 + rec.ts.subsec_nanos() as f64 * 1e-9;
            writeln!(f,
                     "{}{:.6}{} {}: {}",
                     color::Fg(color::White),
                     ts,
                     color::Fg(color::Reset),
                     name,
                     data)?;
        }
        Ok(())
    }
}

fn setup_master(stdin: &Stdin) {
    let mut attrs = tcgetattr(stdin.as_raw_fd()).unwrap();
    attrs.input_flags.insert(InputFlags::IGNBRK);
    attrs.input_flags.remove(InputFlags::BRKINT);
    attrs.input_flags.remove(InputFlags::IXON);
    attrs.input_flags.remove(InputFlags::ICRNL);
    attrs.input_flags.remove(InputFlags::IMAXBEL);
    // attrs.local_flags.remove(LocalFlags::ECHOKE); //      0x00000001  /* visual erase for line kill */
    attrs.local_flags.remove(LocalFlags::ECHOE); //       0x00000002  /* visually erase chars */
    attrs.local_flags.remove(LocalFlags::ECHOK); //       0x00000004  /* echo NL after line kill */
    attrs.local_flags.remove(LocalFlags::ECHO); //        0x00000008  /* enable echoing */
    // attrs.local_flags.remove(LocalFlags::ECHONL); //      0x00000010  /* echo NL even if ECHO is off */
    // attrs.local_flags.remove(LocalFlags::ECHOPRT); //     0x00000020  /* visual erase mode for hardcopy */
    // attrs.local_flags.remove(LocalFlags::ECHOCTL); //     0x00000040  /* echo control chars as ^(Char) */
    attrs.local_flags.remove(LocalFlags::ISIG); //        0x00000080  /* enable signals INTR, QUIT, [D]SUSP */
    attrs.local_flags.remove(LocalFlags::ICANON); //      0x00000100  /* canonicalize input lines */
    // attrs.local_flags.remove(LocalFlags::ALTWERASE); //   0x00000200  /* use alternate WERASE algorithm */
    attrs.local_flags.remove(LocalFlags::IEXTEN); //      0x00000400  /* enable DISCARD and LNEXT */
    // attrs.local_flags.remove(LocalFlags::EXTPROC); //         0x00000800      /* external processing */
    // attrs.local_flags.remove(LocalFlags::TOSTOP); //      0x00400000  /* stop background jobs from output */
    // attrs.local_flags.remove(LocalFlags::FLUSHO); //      0x00800000  /* output being flushed (state) */
    // attrs.local_flags.remove(LocalFlags::NOKERNINFO); //  0x02000000  /* no kernel output from VSTATUS */
    attrs.local_flags.remove(LocalFlags::PENDIN); //      0x20000000  /* XXX retype pending input (state) */
    // attrs.local_flags.remove(LocalFlags::NOFLSH); //      0x80000000  /* don't flush after interrupt */
    tcsetattr(stdin.as_raw_fd(), SetArg::TCSANOW, &attrs).unwrap();
}

struct Piper {
    buf: Vec<u8>,
    read: RawFd,
    write: RawFd,
}

fn errno(e: nix::Error) -> nix::errno::Errno {
    match e {
        nix::Error::Sys(errno) => errno,
        nix::Error::InvalidPath => panic!(),
        nix::Error::InvalidUtf8 => panic!(),
        nix::Error::UnsupportedOperation => panic!(),
    }
}

struct EofData {
    data: Vec<u8>,
    eof: bool,
}

impl Piper {
    fn new(read: RawFd, write: RawFd) -> Piper {
        Piper {
            buf: Vec::with_capacity(4096),
            read,
            write,
        }
    }

    fn pipe_some(&mut self) -> Result<EofData, Error> {
        let mut chunk = Vec::new();

        let mut eof = false;

        loop {
            let mut offset = 0;
            while offset < self.buf.len() {
                match nix::unistd::write(self.write, &self.buf[offset..]) {
                    Err(e) => {
                        self.buf = self.buf[offset..].to_vec();
                        if errno(e) == nix::errno::Errno::EAGAIN {
                            break;
                        } else {
                            return Err(e.into());
                        }
                    }
                    Ok(0) => panic!(),
                    Ok(n) => {
                        chunk.extend(&self.buf[offset..offset + n]);
                        offset += n;
                    }
                }
            }

            unsafe {
                self.buf.set_len(4096);
            }
            match nix::unistd::read(self.read, &mut self.buf) {
                Err(e) => {
                    self.buf.truncate(0);
                    if errno(e) == nix::errno::Errno::EAGAIN {
                        break;
                    } else {
                        return Err(e.into());
                    }
                }
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    self.buf.truncate(n);
                }
            }
        }

        Ok(EofData {
            data: chunk,
            eof,
        })
    }
}

fn set_no_block(fd: RawFd) {
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK)).unwrap();
}

fn record<It: Iterator<Item = S>, S: AsRef<OsStr>>(mut cmd: It) -> Result<Recording, Error> {
    let winsize = Winsize {
        ws_row: 100,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&winsize), None)?;

    let begin = Instant::now();

    let mut records = Vec::new();


    // let records_clone = records.clone();
    // thread::spawn(move || {
    //     let mut config: Attrs = tcgetattr(master_fd).unwrap().into();

    //     records_clone
    //         .lock()
    //         .unwrap()
    //         .push(Record {
    //                   ts: begin - begin,
    //                   event: Event::Config(config.clone()),
    //               });

    //     loop {
    //         thread::sleep(Duration::from_millis(100));
    //         let new_config: Attrs = tcgetattr(master_fd).unwrap().into();
    //         let ts = Instant::now();

    //         if &new_config != &config {
    //             records_clone
    //                 .lock()
    //                 .unwrap()
    //                 .push(Record {
    //                           ts: ts - begin,
    //                           event: Event::Config(new_config.clone()),
    //                       });
    //             config = new_config;
    //         }
    //     }
    // });

    let head = cmd.next().unwrap();

    let slave = pty.slave;
    let master = pty.master;

    let stdin = io::stdin();
    setup_master(&stdin);
    let stdout = io::stdout();

    set_no_block(stdin.as_raw_fd());
    set_no_block(stdout.as_raw_fd());
    set_no_block(master);


    let mut child = pr::Command::new(head)
        .args(cmd)
        .stdin(unsafe { pr::Stdio::from_raw_fd(slave) })
        .stdout(unsafe { pr::Stdio::from_raw_fd(slave) })
        .stderr(unsafe { pr::Stdio::from_raw_fd(slave) })
        .before_exec(move || {
            let res = unsafe { nix::libc::setsid() };
            assert!(res >= 0,
                    "Failed to set session id: {}",
                    nix::errno::errno());

            let res = unsafe { ioctl(slave, TIOCSCTTY as _, 0) };
            assert_eq!(res, 0, "{}", nix::errno::errno());

            unsafe {
                nix::libc::close(slave);
                nix::libc::close(master);
            }

            Ok(())
        })
        .spawn()?;

    // output_thread.join().unwrap();

    let mut input = Piper::new(stdin.as_raw_fd(), master);
    let mut output = Piper::new(master, stdout.as_raw_fd());

    loop {
        let mut read_set = FdSet::new();
        read_set.insert(master);
        read_set.insert(stdin.as_raw_fd());

        let mut write_set = FdSet::new();
        if input.buf.len() > 0 {
            write_set.insert(master);
        }
        if output.buf.len() > 0 {
            write_set.insert(stdout.as_raw_fd());
        }

        let mut except_set = FdSet::new();
        except_set.insert(master);
        except_set.insert(stdin.as_raw_fd());
        except_set.insert(stdout.as_raw_fd());

        let _res = select(
            None,
            Some(&mut read_set),
            Some(&mut write_set),
            Some(&mut except_set),
            None).unwrap();

        // eprintln!("select");

        let ts = Instant::now();

        if read_set.contains(stdin.as_raw_fd()) || write_set.contains(master) {
            // eprintln!("a");
            let r = input.pipe_some()?;
            records.push(Record {
                ts: ts - begin,
                event: Event::Input(r.data),
            });
            if r.eof {
                panic!();
            }
        }

        if read_set.contains(master) || write_set.contains(stdout.as_raw_fd()) {
            // eprintln!("b");
            let r = output.pipe_some()?;
            records.push(Record {
                ts: ts - begin,
                event: Event::Output(r.data),
            });
            if r.eof {
                break;
            }
        }
    }

    // eprintln!("exit status: {:?}", child.wait()?);

    Ok(Recording { records })
}

fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let recording = record(args.iter())?;

    eprintln!("{}", recording);

    Ok(())
}

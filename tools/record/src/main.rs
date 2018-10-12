
use std::ffi::OsStr;
use std::io::Write;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process as pr;
use std::fs::File;
use std::{io, fmt, str, thread};
use std::sync::{Arc, Mutex};
use std::time::{Instant, Duration};
use std::env;

use failure::Error;
use nix::pty::openpty;
use nix::pty::Winsize;
use nix::sys::termios::{tcgetattr, Termios, SetArg, InputFlags, LocalFlags};
use nix::sys::termios::tcsetattr;
use nix::libc::TIOCSCTTY;
use nix::libc::ioctl;
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
        println!("print(ch: {:?})", ch);
    }

    fn execute(&mut self, byte: u8) {
        println!("execute(byte: {:x?})", byte);
    }

    fn hook(&mut self, params: &[i64], intermediates: &[u8], ignore: bool) {
        println!("hook(params: {:?}, intermediates: {:?}, ignore: {:?})", params, intermediates, ignore);
    }

    fn put(&mut self, byte: u8) {
        println!("put(byte: {:x?})", byte);
    }

    fn unhook(&mut self) {
        println!("unhook()");
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        println!("osc_dispatch(params: {:?})", params);
    }

    fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], ignore: bool, ch: char) {
        println!("csi_dispatch(params: {:?}, intermediates: {:?}, ignore: {:?}, ch: {:?})",
            params, intermediates, ignore, ch);
    }


    fn esc_dispatch(&mut self, params: &[i64], intermediates: &[u8], ignore: bool, byte: u8) {
        println!("esc_dispatch(params: {:?}, intermediates: {:?}, ignore: {:?}, byte: {:x?})",
            params,
            intermediates,
            ignore,
            byte);
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

fn record<It: Iterator<Item = S>, S: AsRef<OsStr>>(mut cmd: It) -> Result<Recording, Error> {
    let winsize = Winsize {
        ws_row: 100,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&winsize), None)?;

    let begin = Instant::now();

    let records = Arc::new(Mutex::new(Vec::new()));

    let master_fd = pty.master;
    let mut master_read = unsafe { File::from_raw_fd(pty.master) };
    let mut master_write = unsafe { File::from_raw_fd(pty.master) };

    let records_clone = records.clone();
    let output_thread = thread::spawn(move || {
        let mut stdout = io::stdout();
        loop {
            let mut data = Vec::with_capacity(4096);
            data.resize(4096, 0);

            let len = master_read.read(&mut data).unwrap();
            let ts = Instant::now();
            if len > 0 {
                data.truncate(len);

                stdout.write_all(&data).unwrap();

                records_clone
                    .lock()
                    .unwrap()
                    .push(Record {
                              ts: ts - begin,
                              event: Event::Output(data),
                          });
            } else {
                break;
            }
        }
    });

    let records_clone = records.clone();
    let _input_thread = thread::spawn(move || {
        let mut stdin = io::stdin();

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

        println!("before: {:?}", Attrs::from(attrs.clone()));
        // cfmakeraw(&mut attrs);
        println!("after: {:?}", Attrs::from(attrs.clone()));

        let mut p = vte::Parser::new();
        for byte in b"\x1b[?1049h\x1b[?1h1b=\x0d" {
            p.advance(&mut PrintPerformer, *byte);
        }

        tcsetattr(stdin.as_raw_fd(), SetArg::TCSANOW, &attrs).unwrap();


        loop {
            let mut data = Vec::with_capacity(4096);
            data.resize(4096, 0);

            let len = stdin.read(&mut data).unwrap();
            let ts = Instant::now();
            if len > 0 {
                data.truncate(len);

                master_write.write_all(&data).unwrap();

                records_clone
                    .lock()
                    .unwrap()
                    .push(Record {
                              ts: ts - begin,
                              event: Event::Input(data),
                          });
            } else {
                break;
            }
        }
    });

    let records_clone = records.clone();
    thread::spawn(move || {
        let mut config: Attrs = tcgetattr(master_fd).unwrap().into();

        records_clone
            .lock()
            .unwrap()
            .push(Record {
                      ts: begin - begin,
                      event: Event::Config(config.clone()),
                  });

        loop {
            thread::sleep(Duration::from_millis(100));
            let new_config: Attrs = tcgetattr(master_fd).unwrap().into();
            let ts = Instant::now();

            if &new_config != &config {
                records_clone
                    .lock()
                    .unwrap()
                    .push(Record {
                              ts: ts - begin,
                              event: Event::Config(new_config.clone()),
                          });
                config = new_config;
            }
        }
    });

    let head = cmd.next().unwrap();

    let slave = pty.slave;
    let master = pty.master;

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

    println!("exit status: {:?}", child.wait()?);

    output_thread.join().unwrap();

    let records = records.lock().unwrap().clone();

    Ok(Recording { records })
}

fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let recording = record(args.iter())?;

    println!("{}", recording);

    Ok(())
}

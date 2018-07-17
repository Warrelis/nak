#[macro_use]
extern crate failure;
#[macro_use]
extern crate serde_derive;
extern crate os_pipe;
extern crate futures;
extern crate tokio;
extern crate base64;
extern crate liner;
extern crate serde;
extern crate serde_json;
extern crate ctrlc;
extern crate libc;
extern crate termion;

use termion::input::TermRead;
use termion::raw::IntoRawMode;
use liner::{KeyMap, Editor, Buffer, KeyBindings, Emacs};
use liner::EventHandler;
use std::io::stdin;
use std::io::stdout;
use std::io::Write;
use std::str;
use std::io;
use std::env;
use std::sync::mpsc;

use failure::Error;

mod parse;
mod edit;
mod prefs;
mod comm;

use parse::Parser;
use prefs::Prefs;
use comm::{BackendRemote, launch_backend};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Unknown(String, Args),
    SetDirectory(String),
}

impl Command {
    pub fn add_args(&mut self, new_args: Vec<String>) {
        match self {
            &mut Command::Unknown(_, ref mut args) => {
                args.args.extend(new_args)
            }
            &mut Command::SetDirectory(_) => panic!(), 
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Args {
    args: Vec<String>
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Multiplex<M> {
    remote_id: usize,
    message: M
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcRequest {
    BeginCommand {
        id: usize,
        stdout_pipe: usize,
        stderr_pipe: usize,
        command: Command,
    },
    CancelCommand {
        id: usize,
    },
    BeginRemote {
        id: usize,
        command: Command,
    },
    EndRemote {
        id: usize,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RpcResponse {
    Pipe {
        id: usize,
        data: Vec<u8>,
    },
    CommandDone {
        id: usize,
        exit_code: i64,
    },
}

pub struct Pipes {
    id: usize,
    stdout_pipe: usize,
    stderr_pipe: usize,
}

pub struct Remote {
    id: usize,
}

pub enum Event {
    Remote(Multiplex<RpcResponse>),
    CtrlC,
}

#[derive(Clone, Debug, PartialEq)]
enum Shell {
    DoNothing,
    Run(Command),
    BeginRemote(Command),
    Exit,
}

trait Reader {
    fn get_command(&mut self, backend: &BackendRemote) -> Result<Shell, Error>;
    fn save_history(&mut self);
}

fn check_single_arg(items: &[&str]) -> Result<String, Error> {
    if items.len() == 1 {
        Ok(items[0].to_string())
    } else {
        Err(format_err!("Bad argument length: {}", items.len()))
    }
}

fn parse_command_simple(prefs: &Prefs, input: &str) -> Result<Shell, Error> {
    let mut p = Parser::new();

    if input.len() == 0 {
        return Ok(Shell::DoNothing);
    }

    let cmd = p.parse(input);

    let head = cmd.head();

    if let Some(mut new_cmd) = prefs.expand(head) {
        new_cmd.add_args(cmd.body().into_iter().map(String::from).collect());
        return Ok(Shell::Run(new_cmd));
    }

    Ok(Shell::Run(match head {
        "cd" => Command::SetDirectory(check_single_arg(&cmd.body())?),
        "nak" => {
            let mut it = cmd.body().into_iter();
            let head = it.next().unwrap().to_string();

            return Ok(Shell::BeginRemote(Command::Unknown(
                head,
                Args { args: it.map(String::from).collect() },
            )));
        }
        _ => Command::Unknown(
            head.to_string(),
            Args { args: cmd.body().into_iter().map(String::from).collect() },
        )
    }))
}

struct SimpleCompleter;

impl liner::Completer for SimpleCompleter {
    fn completions(&self, _start: &str) -> Vec<String> {
        vec![]
    }
}

struct SimpleReader {
    prefs: Prefs,
    ctx: liner::Context,
}

impl SimpleReader {
    fn new(prefs: Prefs) -> Result<SimpleReader, Error> {
        let mut history = liner::History::new();
        history.set_file_name(Some(env::home_dir().unwrap().join(".config").join("nak").join("history.nak").into_os_string().into_string().unwrap()));
        match history.load_history() {
            Ok(_) => {}
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
        }
        Ok(SimpleReader {
            prefs,
            ctx: liner::Context {
                history,
                completer: Some(Box::new(SimpleCompleter)),
                word_divider_fn: Box::new(liner::get_buffer_words),
                key_bindings: KeyBindings::Emacs,
            }
        })
    }
}

impl Reader for SimpleReader {
    fn get_command(&mut self, backend: &BackendRemote) -> Result<Shell, Error> {

        fn handle_keys<'a, T, W: Write, M: KeyMap<'a, W, T>>(
            mut keymap: M,
            mut handler: &mut EventHandler<W>,
        ) -> io::Result<String>
        where
            String: From<M>,
        {
            let stdin = stdin();
            for c in stdin.keys() {
                if try!(keymap.handle_key(c.unwrap(), handler)) {
                    break;
                }
            }

            Ok(keymap.into())
        }

        let prompt = format!("[{}]$ ", backend.remotes.len());

        let buffer = Buffer::new();

        let res = {
            let stdout = stdout().into_raw_mode().unwrap();
            let ed = Editor::new_with_init_buffer(stdout, prompt, &mut self.ctx, buffer)?;
            match handle_keys(Emacs::new(ed), &mut |_| {}) {
                Ok(res) => res,
                Err(e) => {
                    return match e.kind() {
                        io::ErrorKind::Interrupted => Ok(Shell::DoNothing),
                        io::ErrorKind::UnexpectedEof => Ok(Shell::Exit),
                        _ => Err(e.into()),
                    }
                }
            }
        };

        let parsed = parse_command_simple(&self.prefs, &res)?;

        self.ctx.history.push(Buffer::from(res))?;

        Ok(parsed)
    }

    fn save_history(&mut self) {
        self.ctx.history.commit_history()
    }
}

#[test]
fn parse_simple() {
    let c = parse_command_simple(&Prefs::default(), " test 1 abc 2").unwrap();
    assert_eq!(c, Shell::Run(Command::Unknown(
        String::from("test"),
        Args {args: vec![
            String::from("1"),
            String::from("abc"),
            String::from("2"),
        ]}
    )));
}

enum TermState {
    ReadingCommand,
    WaitingOn(Pipes),
}

struct Exec {
    receiver: mpsc::Receiver<Event>,
    remote: BackendRemote,
    reader: Box<dyn Reader>,
    state: TermState,
}

impl Exec {
    fn one_loop(&mut self) -> Result<bool, Error> {
        match self.state {
            TermState::ReadingCommand => {
                let cmd = self.reader.get_command(&mut self.remote)?;

                match cmd {
                    Shell::Exit => {
                        if let Some(remote) = self.remote.remotes.pop() {
                            self.remote.end_remote(remote)?;
                        } else {
                            return Ok(false)
                        }
                    }
                    Shell::DoNothing => {}
                    Shell::BeginRemote(cmd) => {
                        let res = self.remote.begin_remote(cmd)?;
                        self.remote.push_remote(res);
                    }
                    Shell::Run(cmd) => {
                        let res = self.remote.run(cmd)?;
                        self.state = TermState::WaitingOn(res);
                    }
                }
            }
            TermState::WaitingOn(Pipes { id, stdout_pipe, stderr_pipe }) => {
                let msg = self.receiver.recv()?;

                match msg {
                    Event::Remote(msg) => {
                        assert_eq!(msg.remote_id, self.remote.remotes.last().map(|x| x.id).unwrap_or(0));

                        match msg.message {
                            RpcResponse::Pipe { id, data } => {
                                if id == stdout_pipe {
                                    io::stdout().write(&data)?;
                                } else if id == stderr_pipe {
                                    io::stderr().write(&data)?;
                                } else {
                                    assert!(false, "{} {} {}", id, stdout_pipe, stderr_pipe);
                                }
                            }
                            RpcResponse::CommandDone { id, exit_code: _ } => {
                                assert_eq!(id, id);
                                // println!("exit_code: {}", exit_code);
                                self.state = TermState::ReadingCommand;
                            }
                        }
                    }
                    Event::CtrlC => {
                        self.remote.cancel(id)?;
                    }
                }
            }
        }
        Ok(true)
    }
}

fn remote_run(receiver: mpsc::Receiver<Event>, remote: BackendRemote, reader: Box<dyn Reader>)
    -> Result<(), Error>
{
    let mut exec = Exec {
        receiver,
        remote,
        reader,
        state: TermState::ReadingCommand,
    };

    while exec.one_loop()? {}

    exec.reader.save_history();

    Ok(())
}

fn main() -> Result<(), Error> {
    let prefs = Prefs::load()?;
    // let remote = Box::new(SimpleRemote);
    let (sender, receiver) = mpsc::channel();

    let sender_clone = sender.clone();

    ctrlc::set_handler(move || {
        sender_clone.send(Event::CtrlC).unwrap();
    }).expect("Error setting CtrlC handler");

    let remote = launch_backend(sender)?;

    remote_run(receiver, remote, Box::new(SimpleReader::new(prefs)?))?;

    Ok(())
}

use prefs::Prefs;
use protocol::Command;
use std::io::Write;
use failure::Error;
use parse::{Ast, Cmd, parse_input};
use comm::BackendEndpoint;
use liner;
use std::env;
use std::io;
use termion::input::TermRead;
use termion::raw::IntoRawMode;
use liner::{KeyMap, Editor, Buffer, KeyBindings, Emacs};
use liner::EventHandler;
use std::io::stdin;
use std::io::stdout;
use {Shell, Plan};


pub trait Reader {
    fn get_command(&mut self, prompt: String, backend: &BackendEndpoint) -> Result<Shell, Error>;
    fn save_history(&mut self);
}

fn check_single_arg<'a>(items: impl Iterator<Item=String>) -> Result<String, Error> {
    let mut items = items;
    if let Some(i) = items.next() {
        if items.next().is_none() {
            Ok(i)
        } else {
            Err(format_err!("bad too many args"))
        }
    } else {
        Err(format_err!("not enough args"))
    }
}

fn convert_single(prefs: &Prefs, cmd: Cmd) -> Result<Shell, Error> {

    let items = prefs.expand(cmd.0.into_iter().map(|w| w.expand_string()).collect());

    let mut it = items.into_iter();
    let head = it.next().unwrap();

    Ok(Shell::Run {
        cmd: match head.as_str() {
            "cd" => Command::SetDirectory(check_single_arg(it)?),
            "micro" => Command::Edit(check_single_arg(it)?), // TODO: make this a configurable alias instead
            "nak" => {
                let head = it.next().unwrap();

                return Ok(Shell::BeginRemote(Command::Unknown(
                    head,
                    it.collect(),
                )));
            }
            _ => Command::Unknown(
                head.to_string(),
                it.collect(),
            )
        },
        redirect: None,
    })
}

fn parse_command_simple(prefs: &Prefs, input: &str) -> Result<Shell, Error> {
    let cmd = parse_input(input);

    match cmd {
        Ast::Empty => Ok(Shell::DoNothing),
        Ast::Cmd(cmd) => convert_single(prefs, cmd),
        Ast::Sequence(_head, _clauses) => {
            // convert_single(prefs, head);
            panic!();
        }
        Ast::Redirect(head, clauses) => {
            let mut p = Plan::new();
            panic!();
        }
    }
}

struct SimpleCompleter;

impl liner::Completer for SimpleCompleter {
    fn completions(&self, _start: &str) -> Vec<String> {
        vec![]
    }
}

pub struct SimpleReader {
    prefs: Prefs,
    ctx: liner::Context,
}

impl SimpleReader {
    pub fn new(prefs: Prefs) -> Result<SimpleReader, Error> {
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
    fn get_command(&mut self, prompt: String, _backend: &BackendEndpoint) -> Result<Shell, Error> {

        fn handle_keys<'a, T, W: Write, M: KeyMap<'a, W, T>>(
            mut keymap: M,
            handler: &mut EventHandler<W>,
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

        match &parsed {
            Shell::DoNothing => {}
            _ => self.ctx.history.push(Buffer::from(res))?,
        }

        Ok(parsed)
    }

    fn save_history(&mut self) {
        self.ctx.history.commit_history()
    }
}

#[test]
fn parse_simple() {
    let c = parse_command_simple(&Prefs::default(), " test 1 abc 2").unwrap();
    assert_eq!(c, Shell::Run {
        cmd: Command::Unknown(
            String::from("test"),
            vec![
                String::from("1"),
                String::from("abc"),
                String::from("2"),
            ],
        ),
        redirect: None,
    });
    let c = parse_command_simple(&Prefs::git_alias_prefs(), "g c -am \"test message\"").unwrap();
    assert_eq!(c, Shell::Run {
        cmd: Command::Unknown(
            String::from("git"),
            vec![
                String::from("commit"),
                String::from("-am"),
                String::from("test message"),
            ],
        ),
        redirect: None,
    });

    let c = parse_command_simple(&Prefs::default(), "").unwrap();
    assert_eq!(c, Shell::DoNothing);

    let c = parse_command_simple(&Prefs::default(), "  ").unwrap();
    assert_eq!(c, Shell::DoNothing);
}




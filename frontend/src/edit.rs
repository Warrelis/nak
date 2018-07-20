use prefs::Prefs;
use Shell;
use protocol::Command;
use std::io::Write;
use failure::Error;
use parse::Parser;
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


pub trait Reader {
    fn get_command(&mut self, backend: &BackendEndpoint) -> Result<Shell, Error>;
    fn save_history(&mut self);
}

fn check_single_arg<'a>(items: &[String]) -> Result<String, Error> {
    if items.len() == 1 {
        Ok(items[0].clone())
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

    let mut parts = vec![cmd.head().to_string()];
    parts.extend(cmd.body().into_iter().map(|e| e.to_string()));

    parts = prefs.expand(parts);

    let redirect = cmd.redirect().map(|r| r.file().to_string());

    let head = &parts[0];
    let body = &parts[1..];

    Ok(Shell::Run {
        cmd: match head.as_str() {
            "cd" => Command::SetDirectory(check_single_arg(body)?),
            "micro" => Command::Edit(check_single_arg(body)?), // TODO: make this a configurable alias instead
            "nak" => {
                let mut it = body.into_iter();
                let head = it.next().unwrap().to_string();

                return Ok(Shell::BeginRemote(Command::Unknown(
                    head,
                    it.map(|i| i.to_string()).collect(),
                )));
            }
            _ => Command::Unknown(
                head.to_string(),
                body.into_iter().map(|i| i.to_string()).collect(),
            )
        },
        redirect,
    })
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
    fn get_command(&mut self, backend: &BackendEndpoint) -> Result<Shell, Error> {

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

        let prompt = format!("[{}]$ ", backend.handler.remotes.len());

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
}

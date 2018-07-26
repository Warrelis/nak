
use std::io::{stdin, stdout, Write};
use std::env;
use std::io;

use failure::Error;
use termion::input::TermRead;
use termion::raw::IntoRawMode;
use liner;
use liner::{KeyMap, Editor, Buffer, KeyBindings, Emacs};
use liner::EventHandler;

use comm::BackendEndpoint;
use parse::{Ast, Cmd, parse_input, Target, Stream};
use protocol::Command;
use prefs::Prefs;
use plan::{PlanBuilder, Plan, Remotes, RemoteRef};

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

fn convert_single(_remotes: &Remotes, prefs: &Prefs, cmd: &Cmd) -> Result<Command, Error> {

    let items = prefs.expand(cmd.words.iter().map(|w| w.expand_string()).collect());

    let mut it = items.into_iter();
    let head = it.next().unwrap();

    Ok(match head.as_str() {
        "cd" => Command::SetDirectory(check_single_arg(it)?),
        "micro" => Command::Edit(check_single_arg(it)?), // TODO: make this a configurable alias instead
        "nak" => {
            let _head = it.next().unwrap();

            panic!();
            // return Ok(Shell::BeginRemote(Command::Unknown(
            //     head,
            //     it.collect(),
            // )));
        }
        _ => Command::Unknown(
            head.to_string(),
            it.collect(),
        )
    })
}

fn convert_ast(remotes: &Remotes, prefs: &Prefs, ast: &Ast, plan: &mut PlanBuilder, mut stdout: usize, mut stderr: usize) -> Result<usize, Error> {
    Ok(match ast {
        Ast::Empty => stdout,
        Ast::Cmd(cmd) => {
            let stdin = plan.pipe();
            eprintln!("piping {:?}, {}", cmd, stdin);
            plan.add_command(*remotes.stack.last().unwrap(), convert_single(remotes, prefs, cmd)?, stdin, stdout, stderr);
            stdin
        }
        Ast::Sequence(_head, _clauses) => {
            panic!();
        }
        Ast::Redirect(head, clauses) => {
            for clause in clauses.iter().rev() {
                let stdin = match &clause.1 {
                    Target::File(path) => {
                        plan.add_file_output(*remotes.stack.last().unwrap(), path.expand_string())
                    }
                    Target::Command(cmd) => {
                        eprintln!("planning {:?}", cmd);
                        convert_ast(remotes, prefs, &cmd, plan, stdout, stderr)?
                    }
                };

                match clause.0 {
                    Stream::Stdout => {
                        stdout = stdin;
                        stderr = plan.pipe();
                    }
                    Stream::Stderr => {
                        stderr = stdin;
                        stdout = plan.pipe();
                    }
                }
            }

            convert_ast(remotes, prefs, head.as_ref(), plan, stdout, stderr)?
        }
    })
}

fn parse_command_simple(remotes: &Remotes, prefs: &Prefs, input: &str) -> Result<Plan, Error> {
    let cmd = parse_input(input);

    let mut p = PlanBuilder::new();

    let stdout = p.pipe();
    let stderr = p.pipe();
    let stdin = convert_ast(remotes, prefs, &cmd, &mut p, stdout, stderr)?;

    p.set_stdin(Some(stdin));
    p.set_stdout(Some(stdout));
    p.set_stderr(Some(stderr));

    Ok(p.build())
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

impl SimpleReader {
    pub fn get_command(&mut self, prompt: String, _backend: &BackendEndpoint) -> Result<Plan, Error> {

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
                        io::ErrorKind::Interrupted => Ok(Plan::empty()),
                        io::ErrorKind::UnexpectedEof => {
                            let mut p = PlanBuilder::new();
                            p.exit(RemoteRef(0));

                            Ok(p.build())
                        }
                        _ => Err(e.into()),
                    }
                }
            }
        };
        let remotes = &Remotes {
            stack: vec![RemoteRef(0)],
        };

        let parsed = parse_command_simple(&remotes, &self.prefs, &res)?;

        // match &parsed {
        //     Shell::DoNothing => {}
        //     _ => self.ctx.history.push(Buffer::from(res))?,
        // }
        self.ctx.history.push(Buffer::from(res))?;

        Ok(parsed)
    }

    pub fn save_history(&mut self) {
        self.ctx.history.commit_history()
    }
}

#[test]
fn parse_simple() {
    // let c = parse_command_simple(&Prefs::default(), " test 1 abc 2").unwrap();
    // assert_eq!(c, Plan {
    //     cmd: Command::Unknown(
    //         String::from("test"),
    //         vec![
    //             String::from("1"),
    //             String::from("abc"),
    //             String::from("2"),
    //         ],
    //     ),
    //     redirect: None,
    // });
    // let c = parse_command_simple(&Prefs::git_alias_prefs(), "g c -am \"test message\"").unwrap();
    // assert_eq!(c, Shell::Run {
    //     cmd: Command::Unknown(
    //         String::from("git"),
    //         vec![
    //             String::from("commit"),
    //             String::from("-am"),
    //             String::from("test message"),
    //         ],
    //     ),
    //     redirect: None,
    // });

    let remotes = &Remotes {
        stack: vec![RemoteRef(0)],
    };

    let p = parse_command_simple(remotes, &Prefs::default(), "").unwrap();
    assert_eq!(p, Plan::empty());

    let p = parse_command_simple(remotes, &Prefs::default(), "  ").unwrap();
    assert_eq!(p, Plan::empty());

    let p = parse_command_simple(remotes, &Prefs::default(), "a").unwrap();
    assert_eq!(p, Plan::single(RemoteRef(0), vec![String::from("a")], None));

    let p = parse_command_simple(remotes, &Prefs::default(), "a b c").unwrap();
    assert_eq!(p, Plan::single(RemoteRef(0), vec![String::from("a"), String::from("b"), String::from("c")], None));

    let p = parse_command_simple(remotes, &Prefs::default(), "a>b").unwrap();
    // assert_eq!(p, Plan::single(RemoteRef(0), vec![String::from("a")], Some((RemoteRef(0), String::from("b")))));
}




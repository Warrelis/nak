use std::io;
use std::process as pr;

pub enum Command {
    Tool,
    Path(String),
}

pub struct Args {
    args: Vec<String>
}

#[derive(Debug)]
pub enum Response {
    Text(String),
}

pub trait Remote {
    fn run(&self, c: Command, args: Args) -> Response;
}

pub struct SimpleRemote;

impl Remote for SimpleRemote {
    fn run(&self, c: Command, args: Args) -> Response {
        match c {
            Command::Path(path) => {
                let out = pr::Command::new(path)
                    .args(&args.args)
                    .output()
                    .expect("failed to execute process");

                Response::Text(String::from_utf8(out.stdout).unwrap())
            }
            Command::Tool => panic!(),
        }
    }
}

fn main() {
    let mut input = String::new();

    let remote: Box<dyn Remote> = Box::new(SimpleRemote);

    match io::stdin().read_line(&mut input) {
        Ok(n) => {
            println!("{} bytes read", n);
            println!("{} =>", input);
            let parts = input.trim().split(" ");
            let mut c = None;
            let mut args = Vec::new();
            for p in parts {
                if c.is_none() {
                    c = Some(p.to_string());
                } else {
                    args.push(p.to_string());
                }
            }
            let res = remote.run(Command::Path(c.unwrap()), Args { args });
            match res {
                Response::Text(text) => println!("{}", text),
            }
        }
        Err(error) => println!("error: {}", error),
    }
}

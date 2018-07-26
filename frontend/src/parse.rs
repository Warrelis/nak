
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Target {
    File(Word),
    Command(Ast),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Word {
    Normal(String),
}

impl Word {
    pub fn expand_string(&self) -> String {
        match self {
            Word::Normal(ref s) => s.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SequenceType {
    Wait,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cmd {
    pub remote: Option<String>,
    pub words: Vec<Word>
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceClause(pub SequenceType, pub Ast);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedirectClause(pub Stream, pub Target);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Ast {
    Empty,
    Cmd(Cmd),
    Sequence(Box<Ast>, Vec<SequenceClause>),
    Redirect(Box<Ast>, Vec<RedirectClause>),
}

fn skip_whitespace(input: &mut Consume) {
    while let Some(c) = input.cur() {
        if c != b' ' {
            break;
        }
        input.pos += 1;
    }
}

fn parse_quote(input: &mut Consume) -> Word {
    assert!(input.next() == Some(b'"'));

    let begin = input.pos;

    loop {
        match input.next() {
            Some(b'"') => break,
            Some(_) => {}
            None => panic!(),
        }
    }

    Word::Normal(input.chars[begin..input.pos - 1].to_string())
}

fn parse_word(input: &mut Consume) -> Word {
    if let Some(b'"') = input.cur() {
        return parse_quote(input);
    }

    let begin = input.pos;

    while let Some(ch) = input.cur() {
        match ch {
            b'"' => panic!(),
            b' ' | b';' | b'|' | b'>' => break,
            _ => {
                input.next();
            }
        }
    }

    Word::Normal(input.chars[begin..input.pos].to_string())
}

fn parse_cmd(input: &mut Consume) -> Cmd {
    let mut children = vec![parse_word(input)];

    loop {
        skip_whitespace(input);
        match input.cur() {
            None | Some(b';') | Some(b'|') | Some(b'>') => break,
            _ => children.push(parse_word(input)),
        }
    }

    Cmd {
        remote: None,
        words: children,
    }
}

fn parse_pipe(input: &mut Consume) -> Ast {
    let head = Ast::Cmd(parse_cmd(input));

    let mut children = vec![];

    loop {
        skip_whitespace(input);
        match input.cur() {
            None => break,
            Some(b'|') => {
                input.next();
                skip_whitespace(input);
                children.push(RedirectClause(Stream::Stdout, Target::Command(Ast::Cmd(parse_cmd(input)))));
            }
            Some(b'>') => {
                input.next();
                skip_whitespace(input);
                children.push(RedirectClause(Stream::Stdout, Target::File(parse_word(input))));
            }
            Some(b';') => break,
            Some(ch) => panic!("{}", ch),
        }
    }

    if children.len() == 0 {
        head
    } else {
        Ast::Redirect(Box::new(head), children)
    }
}

fn parse_seq(input: &mut Consume) -> Ast {
    let head = parse_pipe(input);

    let mut children = vec![];

    loop {
        skip_whitespace(input);
        match input.cur() {
            None => break,
            Some(b';') => {
                input.next();
                skip_whitespace(input);
                children.push(SequenceClause(SequenceType::Wait, parse_pipe(input)));
            }
            Some(ch) => panic!("{}", ch),
        }
    }

    if children.len() == 0 {
        head
    } else {
        Ast::Sequence(Box::new(head), children)
    }
}

fn parse_line(input: &mut Consume) -> Ast {
    skip_whitespace(input);

    if input.cur().is_none() {
        return Ast::Empty;
    }

    let res = parse_seq(input);

    assert_eq!(input.pos, input.text.len());

    res
}

pub fn parse_input(input: &str) -> Ast {
    parse_line(&mut Consume { chars: input, text: input.as_bytes(), pos: 0 })
}

struct Consume<'a> {
    chars: &'a str,
    text: &'a [u8],
    pos: usize,
}

impl<'a> Consume<'a> {
    fn cur(&self) -> Option<u8> {
        if self.pos < self.text.len() {
            Some(self.text[self.pos])
        } else {
            None
        }
    }

    fn next(&mut self) -> Option<u8> {
        if self.pos < self.text.len() {
            let res = Some(self.text[self.pos]);
            self.pos += 1;
            res
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json;
    use std::fs::File;
    use std::io::{Read, Write};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct ParserTest {
        input: String,
        output: Option<Ast>,
    }

    #[test]
    fn check_many() {
        let mut contents = String::new();
        File::open("tests/parser.json").unwrap().read_to_string(&mut contents).unwrap();
        let tests: Vec<ParserTest> = serde_json::from_str(&contents).unwrap();

        for test in tests.iter() {
            eprintln!("test: {}", test.input);
            let output = parse_input(&test.input);
            eprintln!("  output: {:#?}", output);

            if let Some(ref expected) = test.output {
                assert_eq!(expected, &output);
            }
        }

        let actual = tests.iter()
            .map(|t| ParserTest {
                input: t.input.clone(),
                output: Some(parse_input(&t.input))
            }).collect::<Vec<_>>();

        File::create("tests/parser.actual.json").unwrap().write_all(serde_json::to_string_pretty(&actual).unwrap().as_bytes()).unwrap();
    }
}

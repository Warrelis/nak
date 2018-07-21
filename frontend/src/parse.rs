use std::borrow::Cow;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeType {
    Whitespace,
    Comment,
    Word,
    Quote,
    Command,
    Sequence,
    SequenceSymbol,
    // If,
    // While,
    // For,
    Pipe,
    PipeSymbol,
    Redirect,
    RedirectSymbol,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    ty: NodeType,
    begin: usize,
    end: usize,
    children: Vec<Node>,
}

impl Node {
    fn assert_full_inner(&self, begin: usize, end: usize) {
        assert_eq!(self.begin, begin);
        assert_eq!(self.end, end);

        if self.children.len() > 0 {
            let mut c = self.begin;
            for ch in &self.children {
                ch.assert_full_inner(c, ch.end);
                c = ch.end;
            }
            assert_eq!(c, self.end);
        }
    }

    fn find_first(&self, ty: NodeType) -> Option<&Node> {
        for c in &self.children {
            if c.ty == ty {
                return Some(c)
            }
        }
        None
    }

    fn find_first_word(&self) -> Option<&Node> {
        for c in &self.children {
            if c.ty == NodeType::Word || c.ty == NodeType::Quote {
                return Some(c)
            }
        }
        None
    }
}

struct View<'t, 'n> {
    node: &'n Node,
    text: &'t str,
}

impl<'t, 'n> View<'t, 'n> {
    fn assert_full(&self) {
        self.node.assert_full_inner(0, self.text.len());
    }

    fn find_first_word(&self) -> Option<View<'t, 'n>> {
        self.node.find_first_word().map(|node| View {
            node,
            text: self.text,
        })
    }

    fn find_first(&self, ty: NodeType) -> Option<View<'t, 'n>> {
        self.node.find_first(ty).map(|node| View {
            node,
            text: self.text,
        })
    }

    fn text(&self) -> &'t str {
        &self.text[self.node.begin..self.node.end]
    }

    fn word_text(&self) -> Cow<'t, str> {
        match self.node.ty {
            NodeType::Word => self.text().into(),
            NodeType::Quote => unquote(self.text()),
            _ => panic!(),
        }
    }
}

fn unquote<'a>(text: &'a str) -> Cow<'a, str> {
    let bytes = text.as_bytes();
    assert!(bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"');

    text[1..text.len() - 1].into()
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PipeDirection {
    ToFile,
    FromFile
}

pub struct Redirect<'t, 'n>(View<'t, 'n>);

impl<'t, 'n> Redirect<'t, 'n> {
    pub fn direction(&self) -> PipeDirection {
        let sym = self.0.find_first(NodeType::RedirectSymbol).unwrap().text();

        match sym {
            ">" => PipeDirection::ToFile,
            "<" => PipeDirection::FromFile,
            _ => panic!(),
        }
    }

    pub fn file(&self) -> Cow<'t, str> {
        self.0.find_first_word().unwrap().word_text()
    }
}

pub struct SequenceView<'t, 'n>(View<'t, 'n>);

impl<'t, 'n> SequenceView<'t, 'n> {
    pub fn commands(&self) -> Vec<CommandView<'t, 'n>> {
        let mut res = Vec::new();
        for c in &self.0.node.children {
            match c.ty {
                NodeType::Whitespace |
                NodeType::SequenceSymbol |
                NodeType::Comment => continue,
                NodeType::Command => res.push(CommandView(View { node: c, text: self.0.text })),
                _ => panic!(),
            }
        }
        res
    }
}

pub struct CommandView<'t, 'n>(View<'t, 'n>);

impl<'t, 'n> CommandView<'t, 'n> {
    pub fn head(&self) -> Cow<'t, str> {
        self.0.find_first(NodeType::Word).map(|v| v.text().into())
            .unwrap_or_else(|| unquote(self.0.find_first(NodeType::Quote).unwrap().text()))
    }

    pub fn body(&self) -> Vec<Cow<'t, str>> {
        let mut res = Vec::new();
        {
            let mut found = false;
            let mut push = |it| {
                if found {
                    res.push(it);
                } else {
                    found = true;
                }
            };
            for c in &self.0.node.children {
                match c.ty {
                    NodeType::Whitespace |
                    NodeType::Comment => continue,
                    NodeType::Redirect => break,
                    NodeType::Word => push(self.0.text[c.begin..c.end].into()),
                    NodeType::Quote => push(unquote(&self.0.text[c.begin..c.end])),
                    _ => panic!(),
                }
            }
        }
        res
    }

    pub fn redirect(&self) -> Option<Redirect<'t, 'n>> {
        self.0.find_first(NodeType::Redirect).map(Redirect)
    }
}

#[derive(Default)]
pub struct Parser {
    nodes: Vec<Node>,
}

fn is_word_char(ch: u8) -> bool {
    match ch {
        b'a'...b'z' | b'A'...b'Z'|b'0'...b'9'|b'.'|b'-'|b'_'|b'/'|b'{'|b'}'|b'$'|b'@'|b'='|b','|b'~' => true,
        _ => false,
    }
}

struct Consume<'a> {
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
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    fn parse_whitespace<'a>(&mut self, input: &mut Consume<'a>) -> Node {
        assert!(input.cur() == Some(b' ') || input.cur() == Some(b'\n'));
        let begin = input.pos;
        input.pos += 1;
        while let Some(c) = input.cur() {
            if c != b' ' {
                break;
            }
            input.pos += 1;
        }
        Node {
            ty: NodeType::Whitespace,
            begin,
            end: input.pos,
            children: vec![],
        }
    }

    fn parse_word<'a>(&mut self, input: &mut Consume<'a>) -> Node {
        match input.cur().unwrap() {
            b'"' => {
                let begin = input.pos;
                input.pos += 1;
                while let Some(c) = input.cur() {
                    if c == b'"' {
                        break;
                    }
                    input.pos += 1;
                }
                input.pos += 1;
                Node {
                    ty: NodeType::Quote,
                    begin,
                    end: input.pos,
                    children: vec![],
                }
            }
            ch => {
                if is_word_char(ch) {
                    let begin = input.pos;
                    input.pos += 1;
                    while let Some(c) = input.cur() {
                        if !is_word_char(c) {
                            break;
                        }
                        input.pos += 1;
                    }
                    Node {
                        ty: NodeType::Word,
                        begin,
                        end: input.pos,
                        children: vec![],
                    }
                } else {
                    panic!();
                }
            }
        }
    }

    fn parse_command<'a>(&mut self, input: &mut Consume<'a>) -> Node {
        let mut children = Vec::new();
        let begin = input.pos;

        while let Some(c) = input.cur() {
            match c {
                b' ' | b'\n' => children.push(self.parse_whitespace(input)),
                b'"' => children.push(self.parse_word(input)),
                b'>' => {
                    let mut redir_children = Vec::new();

                    let begin = input.pos;
                    input.pos += 1;
                    redir_children.push(Node {
                        ty: NodeType::RedirectSymbol,
                        begin,
                        end: input.pos,
                        children: vec![],
                    });

                    match input.cur() {
                        Some(b' ') | Some(b'\n') => redir_children.push(self.parse_whitespace(input)),
                        _ => {}
                    }

                    redir_children.push(self.parse_word(input));

                    children.push(Node {
                        ty: NodeType::Redirect,
                        begin,
                        end: input.pos,
                        children: redir_children,
                    });
                }
                b';' | b'|' => {
                    assert!(children.len() > 0);
                    break;
                }
                _ => children.push(self.parse_word(input)),
            }
        }

        Node {
            ty: NodeType::Command,
            begin: begin,
            end: input.pos,
            children,
        }
    }

    fn parse_pipe<'a>(&mut self, input: &mut Consume<'a>) -> Node {
        let mut children = Vec::new();

        while let Some(c) = input.cur() {
            match c {
                b'>' => panic!(),
                b';' => {
                    assert!(children.len() > 0);
                    break;
                }
                b'|' => {
                    let begin = input.pos;
                    input.pos += 1;

                    children.push(Node {
                        ty: NodeType::PipeSymbol,
                        begin,
                        end: input.pos,
                        children: vec![],
                    });
                }
                _ => children.push(self.parse_command(input)),
            }
        }

        if children.len() == 1 {
            children.into_iter().next().unwrap()
        } else {
            Node {
                ty: NodeType::Pipe,
                begin: 0,
                end: input.text.len(),
                children,
            }
        }
    }

    fn parse_seq<'a>(&mut self, input: &mut Consume<'a>) -> Node {
        let mut children = Vec::new();

        while let Some(c) = input.cur() {
            match c {
                b'>' => panic!(),
                b';' => {
                    let begin = input.pos;
                    input.pos += 1;

                    children.push(Node {
                        ty: NodeType::SequenceSymbol,
                        begin,
                        end: input.pos,
                        children: vec![],
                    });
                }
                _ => children.push(self.parse_pipe(input)),
            }
        }

        Node {
            ty: NodeType::Sequence,
            begin: 0,
            end: input.text.len(),
            children,
        }
    }

    pub fn parse<'t, 'n>(&'n mut self, text: &'t str) -> SequenceView<'t, 'n> {

        let mut input = Consume { text: text.as_bytes(), pos: 0 };

        let cmd = self.parse_seq(&mut input);

        self.nodes.push(cmd);

        let v = View {
            node: self.nodes.last().unwrap(),
            text,
        };

        v.assert_full();

        SequenceView(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json;
    use std::fs::File;
    use std::io::{Read, Write};

    #[test]
    fn check_full() {
        View {
            node: &Node {
                ty: NodeType::Whitespace,
                begin: 0,
                end: 7,
                children: vec![],
            },
            text: "1234567"
        }.assert_full();
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct ParserTest {
        input: String,
        output: Option<Node>,
    }

    #[test]
    fn check_many() {
        let mut contents = String::new();
        File::open("tests/parser.json").unwrap().read_to_string(&mut contents).unwrap();
        let tests: Vec<ParserTest> = serde_json::from_str(&contents).unwrap();

        let mut parser = Parser::new();

        for test in tests.iter() {
            eprintln!("test: {}", test.input);
            let output = parser.parse(&test.input).0;
            eprintln!("  output: {:#?}", output.node);

            output.assert_full();
            if let Some(ref expected) = test.output {
                assert_eq!(expected, output.node);
            }
        }

        let actual = tests.iter()
            .map(|t| ParserTest {
                input: t.input.clone(),
                output: Some(parser.parse(&t.input).0.node.clone())
            }).collect::<Vec<_>>();

        File::create("tests/parser.actual.json").unwrap().write_all(serde_json::to_string_pretty(&actual).unwrap().as_bytes()).unwrap();
    }
}

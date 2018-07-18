use std::borrow::Cow;
use std::borrow::Borrow;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeType {
    Whitespace,
    Comment,
    Word,
    Quote,
    Command,
    Sequence,
    If,
    While,
    For,
    Redirect,
    RedirectSymbol,
    RedirectFile,
}

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

pub struct Command<'t, 'n>(View<'t, 'n>);

impl<'t, 'n> Command<'t, 'n> {
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
        b'a'...b'z' | b'A'...b'Z'|b'0'...b'9'|b'.'|b'-'|b'_'|b'/'|b'{'|b'}'|b'$'|b'@'|b'='|b',' => true,
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
        if let Some(c) = input.cur() {

            match c {
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
        } else {
            panic!();
        }
    }

    pub fn parse<'t, 'n>(&'n mut self, text: &'t str) -> Command<'t, 'n> {

        let mut input = Consume { text: text.as_bytes(), pos: 0 };

        let mut children = Vec::new();

        while let Some(c) = input.cur() {
            match c {
                b' ' | b'\n' => children.push(self.parse_whitespace(&mut input)),
                b'"' => children.push(self.parse_word(&mut input)),
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
                        Some(b' ') | Some(b'\n') => redir_children.push(self.parse_whitespace(&mut input)),
                        _ => {}
                    }

                    redir_children.push(self.parse_word(&mut input));

                    children.push(Node {
                        ty: NodeType::Redirect,
                        begin,
                        end: input.pos,
                        children: redir_children,
                    });
                }
                _ => children.push(self.parse_word(&mut input)),
            }
        }

        self.nodes.push(Node {
            ty: NodeType::Command,
            begin: 0,
            end: input.text.len(),
            children,
        });

        let v = View {
            node: self.nodes.last().unwrap(),
            text,
        };

        v.assert_full();

        Command(v)
    }
}

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

#[test]
fn can_parse() {
    let mut parser = Parser::new();

    parser.parse("");

    {
        let v = parser.parse("test");
        assert_eq!(v.0.node.ty, NodeType::Command);
        assert_eq!(
            v.0.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![NodeType::Word]);
    }

    {
        let v = parser.parse(" test ");
        assert_eq!(v.0.node.ty, NodeType::Command);
        assert_eq!(
            v.0.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![NodeType::Whitespace, NodeType::Word, NodeType::Whitespace]);
    }

    {
        let v = parser.parse(" test test2");
        assert_eq!(v.0.node.ty, NodeType::Command);
        assert_eq!(
            v.0.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![
                NodeType::Whitespace,
                NodeType::Word,
                NodeType::Whitespace,
                NodeType::Word,
            ]);
    }
    {
        let v = parser.parse(" \"test test2\" \"1 2 3\"");
        assert_eq!(v.0.node.ty, NodeType::Command);
        assert_eq!(
            v.0.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![
                NodeType::Whitespace,
                NodeType::Quote,
                NodeType::Whitespace,
                NodeType::Quote,
            ]);
        assert_eq!(v.head(), "test test2");
        assert_eq!(v.body(), vec!["1 2 3"]);
    }
    {
        let v = parser.parse("a \"test test2\" > \"1 2 3\"");
        assert_eq!(v.0.node.ty, NodeType::Command);
        assert_eq!(
            v.0.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![
                NodeType::Word,
                NodeType::Whitespace,
                NodeType::Quote,
                NodeType::Whitespace,
                NodeType::Redirect,
            ]);
        assert_eq!(v.head(), "a");
        assert_eq!(v.body(), vec!["test test2"]);
        let redir = v.redirect().expect("no redir?");
        assert_eq!(redir.direction(), PipeDirection::ToFile);
        assert_eq!(redir.file(), "1 2 3");
    }
}

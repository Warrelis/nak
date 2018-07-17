use std::borrow::Cow;

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
}

struct View<'t, 'n> {
    node: &'n Node,
    text: &'t str,
}

impl<'t, 'n> View<'t, 'n> {
    fn assert_full(&self) {
        self.node.assert_full_inner(0, self.text.len());
    }
}

fn unquote<'a>(text: &'a str) -> Cow<'a, str> {
    let bytes = text.as_bytes();
    assert!(bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"');

    text[1..text.len() - 1].into()
}

pub struct Command<'t, 'n>(View<'t, 'n>);

impl<'t, 'n> Command<'t, 'n> {
    pub fn head(&self) -> Cow<'t, str> {
        for c in &self.0.node.children {
            match c.ty {
                NodeType::Whitespace |
                NodeType::Comment => continue,
                NodeType::Word => return self.0.text[c.begin..c.end].into(),
                NodeType::Quote => return unquote(&self.0.text[c.begin..c.end]),
                _ => panic!(),
            }
        }
        panic!();
    }

    pub fn body(&self) -> Vec<Cow<'t, str>> {
        self.0.node.children.iter()
            .filter(|c| c.ty != NodeType::Whitespace && c.ty != NodeType::Comment)
            .skip(1)
            .map(|c| self.0.text[c.begin..c.end].into())
            .collect()
    }
}

#[derive(Default)]
pub struct Parser {
    nodes: Vec<Node>,
}

fn is_word_char(ch: u8) -> bool {
    match ch {
        b'a'...b'z' | b'A'...b'Z'|b'0'...b'9'|b'.'|b'-' => true,
        _ => false,
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    pub fn parse<'t, 'n>(&'n mut self, input: &'t str) -> Command<'t, 'n> {

        let bytes = input.as_bytes();

        let mut children = Vec::new();

        let mut pos = 0;

        while pos < bytes.len() {
            match bytes[pos] {
                b' ' | b'\n' => {
                    let begin = pos;
                    pos += 1;
                    while pos < bytes.len() && bytes[pos] == b' ' {
                        pos += 1;
                    }
                    children.push(Node {
                        ty: NodeType::Whitespace,
                        begin,
                        end: pos,
                        children: vec![],
                    })
                }
                b'a'...b'z' | b'A'...b'Z'|b'0'...b'9'|b'.'|b'-' => {
                    let begin = pos;
                    pos += 1;
                    while pos < bytes.len() && is_word_char(bytes[pos]) {
                        pos += 1;
                    }
                    children.push(Node {
                        ty: NodeType::Word,
                        begin,
                        end: pos,
                        children: vec![],
                    })
                }
                b'"' => {
                    let begin = pos;
                    pos += 1;
                    while pos < bytes.len() && bytes[pos] != b'"' {
                        pos += 1;
                    }
                    pos += 1;
                    children.push(Node {
                        ty: NodeType::Quote,
                        begin,
                        end: pos,
                        children: vec![],
                    })
                }
                _ => panic!(),
            }
        }

        self.nodes.push(Node {
            ty: NodeType::Command,
            begin: 0,
            end: input.len(),
            children,
        });

        let v = View {
            node: self.nodes.last().unwrap(),
            text: input,
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
        let v = parser.parse(" \"test test2\"");
        assert_eq!(v.0.node.ty, NodeType::Command);
        assert_eq!(
            v.0.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![
                NodeType::Whitespace,
                NodeType::Quote,
            ]);
        assert_eq!(v.head(), "test test2");
    }
}

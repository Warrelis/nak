
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeType {
    Whitespace,
    Comment,
    Word,
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

#[derive(Default)]
struct Parser {
    nodes: Vec<Node>,
}

fn is_word_char(ch: u8) -> bool {
    match ch {
        b'a'...b'z' | b'A'...b'Z'|b'0'...b'9' => true,
        _ => false,
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }
    pub fn parse<'t, 'n>(&'n mut self, input: &'t str) -> View<'t, 'n> {

        let bytes = input.as_bytes();

        let mut children = Vec::new();

        let mut pos = 0;

        while pos < bytes.len() {
            match bytes[pos] {
                b' ' => {
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
                b'a'...b'z' | b'A'...b'Z'|b'0'...b'9' => {
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

        v
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
        assert_eq!(v.node.ty, NodeType::Command);
        assert_eq!(
            v.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![NodeType::Word]);
    }

    {
        let v = parser.parse(" test ");
        assert_eq!(v.node.ty, NodeType::Command);
        assert_eq!(
            v.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![NodeType::Whitespace, NodeType::Word, NodeType::Whitespace]);
    }

    {
        let v = parser.parse(" test test2");
        assert_eq!(v.node.ty, NodeType::Command);
        assert_eq!(
            v.node.children.iter().map(|v| v.ty).collect::<Vec<_>>(),
            vec![
                NodeType::Whitespace,
                NodeType::Word,
                NodeType::Whitespace,
                NodeType::Word,
            ]);
    }
}

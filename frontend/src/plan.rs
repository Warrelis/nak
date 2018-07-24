
use protocol::{Command, Ids};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RemoteRef(pub usize);

pub struct Remotes {
    pub stack: Vec<RemoteRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanProcess {
    id: usize,
    pub stdin: usize,
    pub stdout: usize,
    pub stderr: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RemoteStep {
    OpenInputFile(String),
    OpenOutputFile(String),
    Run(Command, PlanProcess),
    BeginRemote(Command),
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Remote(RemoteRef, RemoteStep),
    Pipe,
    Pager,
    Editor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub stdin: Option<usize>,
    pub stdout: Option<usize>,
    pub stderr: Option<usize>,
}

impl Plan {
    pub fn empty() -> Plan {
        let mut b = PlanBuilder::new();

        let stdin = b.pipe();
        let stdout = b.pipe();
        let stderr = b.pipe();

        b.build(Some(stdin), Some(stdout), Some(stderr))
    }

    pub fn single(remote: RemoteRef, words: Vec<String>, redirect: Option<(RemoteRef, String)>) -> Plan {
        let mut it = words.into_iter();
        let head = it.next().unwrap();
        let rest = it.collect();

        let mut b = PlanBuilder::new();

        let stdin = b.pipe();
        let stderr = b.pipe();

        let (output, stdout) = if let Some((remote, file)) = redirect {
            (b.add_file_output(remote, file), None)
        } else {
            let output = b.pipe();
            (output, Some(output))
        };

        b.add_command(remote, Command::Unknown(head, rest), stdin, output, stderr);

        b.build(Some(stdin), stdout, Some(stderr))
    }
}

pub struct PlanBuilder {
    steps: Vec<Step>,
    proc_ids: Ids,
    pipes: Vec<(bool, bool)>,
}

impl PlanBuilder {
    pub fn new() -> PlanBuilder {
        PlanBuilder {
            steps: Vec::new(),
            proc_ids: Ids::new(),
            pipes: Vec::new(),
        }
    }

    pub fn build(self, stdin: Option<usize>, stdout: Option<usize>, stderr: Option<usize>) -> Plan {
        Plan {
            steps: self.steps,
            stdin,
            stdout,
            stderr,
        }
    }

    pub fn pipe(&mut self) -> usize {
        let id = self.pipes.len();
        self.pipes.push((true, true));
        self.steps.push(Step::Pipe);
        id
    }

    pub fn exit(&mut self, remote: RemoteRef) {
        self.steps.push(Step::Remote(remote, RemoteStep::Close));
    }

    pub fn add_file_output(&mut self, remote: RemoteRef, file: String) -> usize {
        let id = self.pipes.len();
        self.pipes.push((false, true));
        self.steps.push(Step::Remote(remote, RemoteStep::OpenOutputFile(file)));
        id
    }

    fn use_read_end(&mut self, pipe: usize) {
        assert!(self.pipes[pipe].0);
        self.pipes[pipe].0 = false;
    }

    fn use_write_end(&mut self, pipe: usize) {
        assert!(self.pipes[pipe].1);
        self.pipes[pipe].1 = false;
    }

    pub fn add_command(&mut self, remote: RemoteRef, cmd: Command, stdin: usize, stdout: usize, stderr: usize) {
        self.use_read_end(stdin);
        self.use_write_end(stdout);
        self.use_write_end(stderr);
        let id = self.proc_ids.next();
        self.steps.push(Step::Remote(remote, RemoteStep::Run(cmd, PlanProcess {
            id,
            stdin,
            stdout,
            stderr,
        })));
    }
}



use protocol::{Command, Ids};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RemoteRef(pub usize);

pub struct Remotes {
    pub stack: Vec<RemoteRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanProcess {
    id: usize,
    stdin: usize,
    stdout: usize,
    stderr: usize,
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
    Pager,
    Editor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn empty() -> Plan {
        PlanBuilder::new().build()
    }

    pub fn single(remote: RemoteRef, words: Vec<String>, redirect: Option<(RemoteRef, String)>) -> Plan {
        let mut it = words.into_iter();
        let head = it.next().unwrap();
        let rest = it.collect();

        let mut plan = Plan {
            steps: vec![],
        };

        if let Some((file_remote, file)) = redirect {
            plan.steps.push(Step::Remote(file_remote, RemoteStep::OpenOutputFile(file)));
        }

        plan.steps.push(Step::Remote(remote, RemoteStep::Run(Command::Unknown(head, rest), PlanProcess {
            id: 2,
            stdin: 3,
            stdout: 0,
            stderr: 1,
        })));

        plan
    }
}

pub struct PlanBuilder {
    plan: Plan,
    ids: Ids,
    stdout: usize,
    stderr: usize,
    stdin: Option<usize>,
}

impl PlanBuilder {
    pub fn new() -> PlanBuilder {
        let mut ids = Ids::new();
        let stdout = ids.next();
        let stderr = ids.next();
        PlanBuilder {
            plan: Plan {
                steps: Vec::new(),
            },
            ids,
            stdout,
            stderr,
            stdin: None,
        }
    }

    pub fn build(self) -> Plan {
        self.plan
    }

    pub fn stdout(&self) -> usize {
        self.stdout
    }

    pub fn stderr(&self) -> usize {
        self.stderr
    }

    pub fn set_stdin(&mut self, stdin: usize) {
        self.stdin = Some(stdin);
    }

    pub fn exit(&mut self, remote: RemoteRef) {
        self.plan.steps.push(Step::Remote(remote, RemoteStep::Close));
    }

    pub fn add_file_output(&mut self, remote: RemoteRef, file: String) -> usize {
        let id = self.ids.next();
        self.plan.steps.push(Step::Remote(remote, RemoteStep::OpenOutputFile(file)));
        id
    }

    pub fn add_command(&mut self, remote: RemoteRef, cmd: Command, stdout: usize, stderr: usize) -> usize {
        let id = self.ids.next();
        let stdin = self.ids.next();
        self.plan.steps.push(Step::Remote(remote, RemoteStep::Run(cmd, PlanProcess {
            id,
            stdin,
            stdout,
            stderr,
        })));
        stdin
    }
}


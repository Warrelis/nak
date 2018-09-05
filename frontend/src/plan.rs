use std::collections::HashSet;

use protocol::{Command, Ids};

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteRef(pub usize);

pub struct Remotes {
    pub stack: Vec<RemoteRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanProcess {
    id: usize,
    pub stdin: usize,
    pub stdout: usize,
    pub stderr: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RemoteStep {
    OpenInputFile(String),
    OpenOutputFile(String),
    Run(Command, PlanProcess),
    BeginRemote(Command),
    Close,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Step {
    Remote(RemoteRef, RemoteStep),
    Pipe,
    Pager,
    Editor,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub stdin: Option<usize>,
    pub stdout: HashSet<usize>,
    pub stderr: HashSet<usize>,
    pub final_tool: Option<FinalTool>,
}

impl Plan {
    pub fn empty() -> Plan {
        let mut b = PlanBuilder::new();

        let stdout = b.pipe();
        b.set_stdin(Some(stdout));
        b.add_stdout(stdout);
        let stderr = b.pipe();
        b.add_stderr(stderr);

        b.build()
    }

    pub fn single(remote: RemoteRef, words: Vec<String>, redirect: Option<(RemoteRef, String)>) -> Plan {
        let mut it = words.into_iter();
        let head = it.next().unwrap();
        let rest = it.collect();

        let mut b = PlanBuilder::new();

        let stdin = b.pipe();

        let (output, stdout) = if let Some((remote, file)) = redirect {
            (b.add_file_output(remote, file), None)
        } else {
            let output = b.pipe();
            (output, Some(output))
        };
        let stderr = b.pipe();

        b.add_command(remote, Command::Unknown(head, rest), stdin, output, stderr);

        b.set_stdin(Some(stdin));
        if let Some(stdout) = stdout {
            b.add_stdout(stdout);
        }
        b.add_stderr(stderr);

        b.build()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FinalTool {
    Pager,
    Editor,
}

pub struct PlanBuilder {
    plan: Plan,
    proc_ids: Ids,
    pipes: Vec<(bool, bool)>,
}

impl PlanBuilder {
    pub fn new() -> PlanBuilder {
        PlanBuilder {
            plan: Plan {
                steps: Vec::new(),
                stdout: HashSet::new(),
                stderr: HashSet::new(),
                stdin: None,
                final_tool: None,
            },
            proc_ids: Ids::new(),
            pipes: Vec::new(),
        }
    }

    pub fn build(self) -> Plan {
        self.plan
    }

    pub fn pipe(&mut self) -> usize {
        let id = self.pipes.len();
        self.pipes.push((true, true));
        self.plan.steps.push(Step::Pipe);
        id
    }

    pub fn exit(&mut self, remote: RemoteRef) {
        self.plan.steps.push(Step::Remote(remote, RemoteStep::Close));
    }

    pub fn add_file_output(&mut self, remote: RemoteRef, file: String) -> usize {
        let id = self.pipes.len();
        self.pipes.push((false, true));
        self.plan.steps.push(Step::Remote(remote, RemoteStep::OpenOutputFile(file)));
        id
    }

    fn use_read_end(&mut self, pipe: usize) {
        assert!(self.pipes[pipe].0, "{}: {:?}", pipe, self.pipes);
        self.pipes[pipe].0 = false;
    }

    fn use_write_end(&mut self, pipe: usize) {
        assert!(self.pipes[pipe].1, "{}: {:?}", pipe, self.pipes);
        self.pipes[pipe].1 = false;
    }

    pub fn set_stdin(&mut self, stdin: Option<usize>) {
        self.plan.stdin = stdin;
    }

    pub fn add_stdout(&mut self, stdout: usize) {
        self.plan.stdout.insert(stdout);
    }

    pub fn add_stderr(&mut self, stderr: usize) {
        self.plan.stderr.insert(stderr);
    }

    // pub fn set_final_tool(&mut self, final_tool: Option<FinalTool>) {
    //     self.plan.final_tool = final_tool;
    // }

    pub fn add_command(&mut self, remote: RemoteRef, cmd: Command, stdin: usize, stdout: usize, stderr: usize) {
        self.use_read_end(stdin);
        self.use_write_end(stdout);
        self.use_write_end(stderr);
        let id = self.proc_ids.next();
        self.plan.steps.push(Step::Remote(remote, RemoteStep::Run(cmd, PlanProcess {
            id,
            stdin,
            stdout,
            stderr,
        })));
    }
}


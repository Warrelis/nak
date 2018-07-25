
use std::fmt::Debug;
use std::hash::Hash;
use std::collections::{HashMap, HashSet};

use protocol::{Condition, ExitStatus};

struct Waiting<Id: Eq+Hash, Cmd> {
    cmd: Cmd,
    conditions: HashMap<Id, Condition>,
}

pub struct Machine<Id: Eq+Hash+Copy+Clone+Debug, Cmd, State> {
    finished: HashMap<Id, ExitStatus>,
    to_run: HashSet<Id>,
    running: HashMap<Id, State>,
    check_on_completed: HashMap<Id, Vec<(Condition, Id)>>,
    waiting_on: HashMap<Id, Waiting<Id, Cmd>>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Task<Id, Cmd> {
    Start(Id, Cmd),
    ConditionFailed(Id, Cmd),
}

pub enum Status<'a, State: 'a> {
    Running(&'a State),
    Exited(ExitStatus),
}

impl<Id: Eq+Hash+Copy+Clone+Debug, Cmd, State> Machine<Id, Cmd, State> {
    pub fn new() -> Machine<Id, Cmd, State> {
        Machine {
            finished: Default::default(),
            to_run: Default::default(),
            running: Default::default(),
            check_on_completed: Default::default(),
            waiting_on: Default::default(),
        }
    }

    pub fn status<'a>(&'a self, pid: Id) -> Status<'a, State> {
        if let Some(state) = self.running.get(&pid) {
            Status::Running(state)
        } else if let Some(&exit) = self.finished.get(&pid) {
            Status::Exited(exit)
        } else {
            panic!();
        }
    }

    pub fn enqueue(&mut self, new_pid: Id, cmd: Cmd, block_for: HashMap<Id, Condition>) -> Vec<Task<Id, Cmd>> {
        eprintln!("enqueue {:?}", new_pid);
        assert!(
            !self.finished.contains_key(&new_pid) &&
            !self.to_run.contains(&new_pid) &&
            !self.running.contains_key(&new_pid) &&
            !self.waiting_on.contains_key(&new_pid));

        let mut still_needs_to_block_for = HashMap::new();

        for (existing_pid, cond) in block_for {
            if let Some(&status) = self.finished.get(&existing_pid) {
                match cond {
                    Some(expected) => if expected != status {
                        return vec![Task::ConditionFailed(new_pid, cmd)];
                    }
                    None => {}
                }
            } else {
                assert!(
                    self.to_run.contains(&existing_pid) ||
                    self.running.contains_key(&existing_pid) ||
                    self.waiting_on.contains_key(&existing_pid));

                still_needs_to_block_for.insert(existing_pid, cond);
            }
        }

        if still_needs_to_block_for.len() == 0 {
            self.to_run.insert(new_pid);
            vec![Task::Start(new_pid, cmd)]
        } else {
            for (&existing_pid, &cond) in &still_needs_to_block_for {
                self.check_on_completed.entry(existing_pid).or_insert_with(Vec::new).push((cond, new_pid));
            }
            self.waiting_on.insert(new_pid, Waiting {
                cmd,
                conditions: still_needs_to_block_for
            });
            vec![]
        }
    }

    pub fn start(&mut self, pid: Id, state: State) {
        eprintln!("start {:?}", pid);
        assert!(self.to_run.remove(&pid));

        self.running.insert(pid, state);
    }

    pub fn start_completed(&mut self, pid: Id, status: ExitStatus) -> Vec<Task<Id, Cmd>> {
        eprintln!("start_completed {:?}", pid);
        assert!(self.to_run.remove(&pid));

        self.finished.insert(pid, status);
        self.resolve_tasks(pid, status)
    }

    pub fn completed(&mut self, pid: Id, status: ExitStatus) -> Vec<Task<Id, Cmd>> {
        eprintln!("completed {:?}", pid);
        let state = self.running.remove(&pid);
        assert!(state.is_some());
        drop(state);
        self.finished.insert(pid, status);
        self.resolve_tasks(pid, status)
    }

    fn resolve_tasks(&mut self, pid: Id, status: ExitStatus) -> Vec<Task<Id, Cmd>> {
        let mut tasks = Vec::new();
        if let Some(blocked) = self.check_on_completed.remove(&pid) {
            for (cond, waiting_pid) in blocked {
                let mut waiting = self.waiting_on.remove(&waiting_pid).expect("must be waiting");

                match cond {
                    Some(expected) if expected != status =>  {
                        self.finished.insert(waiting_pid, ExitStatus::Failure);
                        tasks.push(Task::ConditionFailed(waiting_pid, waiting.cmd));
                        tasks.extend(self.completed(waiting_pid, ExitStatus::Failure));
                    }
                    _ => {
                        let c2 = waiting.conditions.remove(&pid).expect("was waiting");
                        assert_eq!(c2, cond);

                        if waiting.conditions.len() == 0 {
                            self.to_run.insert(waiting_pid);
                            tasks.push(Task::Start(waiting_pid, waiting.cmd));
                        } else {
                            self.waiting_on.insert(waiting_pid, waiting);
                        }
                    }
                }
            }
        }

        tasks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(items: &[(usize, Option<ExitStatus>)]) -> HashMap<usize, Option<ExitStatus>> {
        let mut res = HashMap::new();

        for item in items {
            res.insert(item.0, item.1);
        }

        res
    }

    #[test]
    fn test_machine() {
        let mut m = Machine::new();

        assert_eq!(m.enqueue(0, "a", wait(&[])), vec![Task::Start(0, "a")]);

        use ExitStatus::*;

        assert_eq!(m.enqueue(1, "b", wait(&[(0, None)])), vec![]);
        m.start(0, "waffle");
        assert_eq!(m.completed(0, ExitStatus::Success), vec![Task::Start(1, "b")]);
     
        assert_eq!(m.enqueue(2, "c", wait(&[(0, None)])), vec![Task::Start(2, "c")]);
        assert_eq!(m.enqueue(3, "d", wait(&[(0, Some(Success))])), vec![Task::Start(3, "d")]);
        assert_eq!(m.enqueue(4, "e", wait(&[(0, Some(Failure))])), vec![Task::ConditionFailed(4, "e")]);
        assert_eq!(m.enqueue(5, "f", wait(&[(2, Some(Success)), (3, Some(Success))])), vec![]);
        assert_eq!(m.enqueue(6, "g", wait(&[(2, Some(Success))])), vec![]);
     
        m.start(2, "badger");
        assert_eq!(m.completed(2, ExitStatus::Success), vec![Task::Start(6, "g")]);
        m.start(3, "anthill");
        assert_eq!(m.completed(3, ExitStatus::Success), vec![Task::Start(5, "f")]);
    }
}
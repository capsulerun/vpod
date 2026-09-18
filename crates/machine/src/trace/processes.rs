use std::collections::{HashMap, VecDeque};

use riscv_core::AT_FDCWD;

const MAX_PROCESSES: usize = 4096;
const MAX_DESCRIPTORS: usize = 1024;
const MAX_PENDING_FORKS: usize = 1024;

#[derive(Clone, Default)]
pub struct Process {
    pub parent: Option<u32>,
    pub working_directory: Option<String>,
    pub descriptors: HashMap<i32, Descriptor>,
}

#[derive(Clone)]
pub struct Descriptor {
    pub path: String,
    pub close_on_exec: bool,
}

pub struct Fork {
    pub state: Process,
    pub parent_task: u64,
}

#[derive(Default)]
pub struct Processes {
    live: HashMap<u32, Process>,
    arrival: VecDeque<u32>,
    forks: HashMap<u32, Fork>,
    fork_arrival: VecDeque<u32>,
}

impl Processes {
    pub fn contains(&self, process_id: u32) -> bool {
        self.live.contains_key(&process_id)
    }

    pub fn get(&self, process_id: u32) -> Option<&Process> {
        self.live.get(&process_id)
    }

    pub fn get_mut(&mut self, process_id: u32) -> Option<&mut Process> {
        self.live.get_mut(&process_id)
    }

    pub fn insert(&mut self, process_id: u32, state: Process) {
        if self.live.insert(process_id, state).is_none() {
            self.arrival.push_back(process_id);
        }
        while self.arrival.len() > MAX_PROCESSES {
            if let Some(oldest) = self.arrival.pop_front() {
                self.live.remove(&oldest);
            }
        }
    }

    pub fn remove(&mut self, process_id: u32) {
        self.live.remove(&process_id);
        self.forks.remove(&process_id);
    }

    pub fn record_fork(&mut self, child_id: u32, state: Process, parent_task: u64) {
        self.forks.insert(child_id, Fork { state, parent_task });
        self.fork_arrival.push_back(child_id);
        while self.fork_arrival.len() > MAX_PENDING_FORKS {
            if let Some(oldest) = self.fork_arrival.pop_front() {
                self.forks.remove(&oldest);
            }
        }
    }

    pub fn take_fork(&mut self, child_id: u32) -> Option<Fork> {
        self.forks.remove(&child_id)
    }

    pub fn parent_task_of_fork(&self, child_id: u32) -> Option<u64> {
        self.forks.get(&child_id).map(|fork| fork.parent_task)
    }

    pub fn set_working_directory(&mut self, process_id: u32, path: String) {
        self.live.entry(process_id).or_default().working_directory = Some(path);
        if !self.arrival.contains(&process_id) {
            self.arrival.push_back(process_id);
        }
    }

    pub fn resolve(
        &self,
        process_id: Option<u32>,
        directory_fd: i32,
        path: &str,
    ) -> Option<String> {
        if path.starts_with('/') {
            return Some(normalize(path));
        }

        let process = self.live.get(&process_id?)?;
        let base = if directory_fd == AT_FDCWD {
            process.working_directory.as_deref()?
        } else {
            process.descriptors.get(&directory_fd)?.path.as_str()
        };

        if path.is_empty() {
            return Some(normalize(base));
        }
        Some(normalize(&format!("{base}/{path}")))
    }

    pub fn path_of_descriptor(&self, process_id: Option<u32>, fd: i32) -> Option<String> {
        let process = self.live.get(&process_id?)?;
        Some(process.descriptors.get(&fd)?.path.clone())
    }
}

impl Process {
    pub fn open(&mut self, fd: i32, path: String, close_on_exec: bool) {
        if self.descriptors.len() >= MAX_DESCRIPTORS && !self.descriptors.contains_key(&fd) {
            return;
        }
        self.descriptors.insert(
            fd,
            Descriptor {
                path,
                close_on_exec,
            },
        );
    }

    pub fn duplicate(&mut self, from_fd: i32, to_fd: i32, close_on_exec: bool) {
        if let Some(source) = self.descriptors.get(&from_fd) {
            let path = source.path.clone();
            self.open(to_fd, path, close_on_exec);
        } else {
            self.descriptors.remove(&to_fd);
        }
    }

    pub fn close(&mut self, fd: i32) {
        self.descriptors.remove(&fd);
    }

    pub fn close_range(&mut self, first: u32, last: u32) {
        self.descriptors
            .retain(|fd, _| *fd < first as i32 || *fd > last.min(i32::MAX as u32) as i32);
    }

    pub fn keep_across_exec(&mut self) {
        self.descriptors
            .retain(|_, descriptor| !descriptor.close_on_exec);
    }
}

pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }

    let mut normalized = String::with_capacity(path.len());
    for part in parts {
        normalized.push('/');
        normalized.push_str(part);
    }
    if normalized.is_empty() {
        normalized.push('/');
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_at(working_directory: &str) -> Process {
        Process {
            working_directory: Some(working_directory.to_string()),
            ..Process::default()
        }
    }

    #[test]
    fn a_relative_path_lands_under_the_working_directory() {
        let mut processes = Processes::default();
        processes.insert(600, process_at("/app"));

        assert_eq!(
            processes
                .resolve(Some(600), AT_FDCWD, "src/main.rs")
                .as_deref(),
            Some("/app/src/main.rs")
        );
    }

    #[test]
    fn dot_and_dot_dot_are_folded_without_touching_the_guest() {
        assert_eq!(normalize("/app/./src/../build//out"), "/app/build/out");
        assert_eq!(normalize("/.."), "/");
        assert_eq!(normalize("/"), "/");
    }

    #[test]
    fn a_relative_path_under_a_directory_descriptor_uses_that_directory() {
        let mut processes = Processes::default();
        let mut process = process_at("/app");
        process.open(7, "/etc/ssl".to_string(), false);
        processes.insert(600, process);

        assert_eq!(
            processes.resolve(Some(600), 7, "certs/ca.pem").as_deref(),
            Some("/etc/ssl/certs/ca.pem")
        );
        assert_eq!(processes.resolve(Some(600), 9, "certs/ca.pem"), None);
    }

    #[test]
    fn an_absolute_path_needs_no_process_at_all() {
        let processes = Processes::default();

        assert_eq!(
            processes.resolve(None, AT_FDCWD, "/etc/passwd").as_deref(),
            Some("/etc/passwd")
        );
        assert_eq!(processes.resolve(None, AT_FDCWD, "notes.txt"), None);
    }

    #[test]
    fn an_empty_path_names_the_descriptor_itself() {
        let mut processes = Processes::default();
        let mut process = process_at("/app");
        process.open(3, "/usr/bin/python3".to_string(), false);
        processes.insert(600, process);

        assert_eq!(
            processes.resolve(Some(600), 3, "").as_deref(),
            Some("/usr/bin/python3")
        );
    }

    #[test]
    fn descriptors_that_close_on_exec_do_not_survive_it() {
        let mut process = process_at("/app");
        process.open(3, "/tmp/kept".to_string(), false);
        process.open(4, "/tmp/dropped".to_string(), true);

        process.keep_across_exec();

        assert!(process.descriptors.contains_key(&3));
        assert!(!process.descriptors.contains_key(&4));
    }

    #[test]
    fn duplicating_a_descriptor_copies_its_path_and_clears_a_stale_target() {
        let mut process = process_at("/app");
        process.open(3, "/tmp/data".to_string(), false);
        process.open(9, "/tmp/old".to_string(), false);

        process.duplicate(3, 4, true);
        process.duplicate(5, 9, false);

        assert_eq!(process.descriptors[&4].path, "/tmp/data");
        assert!(process.descriptors[&4].close_on_exec);
        assert!(!process.descriptors.contains_key(&9));
    }

    #[test]
    fn closing_a_range_forgets_every_descriptor_in_it() {
        let mut process = process_at("/app");
        process.open(3, "/tmp/a".to_string(), false);
        process.open(4, "/tmp/b".to_string(), false);
        process.open(9, "/tmp/c".to_string(), false);

        process.close_range(3, 5);

        assert!(!process.descriptors.contains_key(&3));
        assert!(!process.descriptors.contains_key(&4));
        assert!(process.descriptors.contains_key(&9));
    }

    #[test]
    fn the_oldest_process_gives_way_once_the_table_is_full() {
        let mut processes = Processes::default();
        for pid in 0..MAX_PROCESSES as u32 + 10 {
            processes.insert(pid, process_at("/app"));
        }

        assert!(!processes.contains(0));
        assert!(processes.contains(MAX_PROCESSES as u32 + 9));
    }
}

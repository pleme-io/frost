//! Job control — tracking background and suspended processes.
//!
//! Uses [`crate::sys`] for wait operations.

use nix::unistd::Pid;

use crate::sys;

/// Status of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Running in the background.
    Running,
    /// Stopped by a signal (e.g. SIGTSTP).
    Stopped,
    /// Terminated with an exit code.
    Done(i32),
    /// Killed by a signal.
    Signaled(i32),
}

/// A single job tracked by the shell.
#[derive(Debug, Clone)]
pub struct Job {
    /// Job number (1-indexed, displayed as `[1]`, `[2]`, etc.).
    pub id: usize,
    /// Process ID of the job leader.
    pub pid: Pid,
    /// Process group ID.
    pub pgid: Pid,
    /// Current status.
    pub status: JobStatus,
    /// The command string (for display in `jobs` output).
    pub command: String,
}

/// A table of active jobs.
#[derive(Debug, Default)]
pub struct JobTable {
    jobs: Vec<Job>,
    next_id: usize,
}

impl JobTable {
    /// Create an empty job table.
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
        }
    }

    /// Add a new job and return its id.
    pub fn add(&mut self, pid: Pid, pgid: Pid, command: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            pid,
            pgid,
            status: JobStatus::Running,
            command,
        });
        id
    }

    /// Remove a job by id.
    pub fn remove(&mut self, id: usize) -> Option<Job> {
        if let Some(pos) = self.jobs.iter().position(|j| j.id == id) {
            Some(self.jobs.remove(pos))
        } else {
            None
        }
    }

    /// Look up a job by id.
    pub fn get(&self, id: usize) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    /// Look up a job by id (mutable).
    pub fn get_mut(&mut self, id: usize) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// Look up a job by its leader PID.
    pub fn find_by_pid(&self, pid: Pid) -> Option<&Job> {
        self.jobs.iter().find(|j| j.pid == pid)
    }

    /// All jobs.
    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter()
    }

    /// Number of tracked jobs.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Wait for a specific job to finish or stop.
    ///
    /// Returns the final status. Updates the job entry in-place.
    pub fn wait_for(&mut self, id: usize) -> Option<JobStatus> {
        let job = self.jobs.iter_mut().find(|j| j.id == id)?;

        match sys::wait_pid(job.pid) {
            Ok(sys::ChildStatus::Exited(code)) => {
                job.status = JobStatus::Done(code);
            }
            Ok(sys::ChildStatus::Signaled(code)) => {
                job.status = JobStatus::Signaled(code);
            }
            Ok(sys::ChildStatus::Stopped) => {
                job.status = JobStatus::Stopped;
            }
            _ => {}
        }

        Some(job.status)
    }

    /// Non-blocking reap of any finished jobs.
    pub fn reap_finished(&mut self) -> Vec<usize> {
        let mut finished = Vec::new();

        for job in &mut self.jobs {
            if job.status != JobStatus::Running {
                continue;
            }
            match sys::try_wait_pid(job.pid) {
                Ok(sys::ChildStatus::Exited(code)) => {
                    job.status = JobStatus::Done(code);
                    finished.push(job.id);
                }
                Ok(sys::ChildStatus::Signaled(code)) => {
                    job.status = JobStatus::Signaled(code);
                    finished.push(job.id);
                }
                Ok(sys::ChildStatus::Stopped) => {
                    job.status = JobStatus::Stopped;
                    finished.push(job.id);
                }
                _ => {}
            }
        }

        finished
    }

    /// Remove all jobs that have completed (Done or Signaled).
    pub fn prune_done(&mut self) {
        self.jobs
            .retain(|j| !matches!(j.status, JobStatus::Done(_) | JobStatus::Signaled(_)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Pid;

    #[test]
    fn add_and_lookup() {
        let mut table = JobTable::new();
        let id = table.add(Pid::from_raw(1234), Pid::from_raw(1234), "sleep 10".into());
        assert_eq!(id, 1);
        assert_eq!(table.len(), 1);

        let job = table.get(id).unwrap();
        assert_eq!(job.pid, Pid::from_raw(1234));
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.command, "sleep 10");
    }

    #[test]
    fn remove_job() {
        let mut table = JobTable::new();
        let id = table.add(Pid::from_raw(100), Pid::from_raw(100), "echo".into());
        assert!(table.remove(id).is_some());
        assert!(table.is_empty());
    }

    #[test]
    fn find_by_pid() {
        let mut table = JobTable::new();
        table.add(Pid::from_raw(42), Pid::from_raw(42), "ls".into());
        assert!(table.find_by_pid(Pid::from_raw(42)).is_some());
        assert!(table.find_by_pid(Pid::from_raw(99)).is_none());
    }

    #[test]
    fn prune_done() {
        let mut table = JobTable::new();
        let id1 = table.add(Pid::from_raw(1), Pid::from_raw(1), "a".into());
        let _id2 = table.add(Pid::from_raw(2), Pid::from_raw(2), "b".into());

        table.get_mut(id1).unwrap().status = JobStatus::Done(0);
        table.prune_done();
        assert_eq!(table.len(), 1);
        assert!(table.get(id1).is_none());
    }

    #[test]
    fn sequential_ids() {
        let mut table = JobTable::new();
        let id1 = table.add(Pid::from_raw(1), Pid::from_raw(1), "a".into());
        let id2 = table.add(Pid::from_raw(2), Pid::from_raw(2), "b".into());
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }
}

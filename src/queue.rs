use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};

use crate::config::{QueueKind, RepoConfig};

pub(crate) struct QueueJob {
    pub repo_id: String,
    pub owner: String,
    pub repo: String,
    pub req_time: Instant,
    pub event_type: String,
    pub json_str: String,
    pub rconf: RepoConfig,
}

impl QueueJob {
    pub fn new(
        repo_id: String,
        owner: String,
        repo: String,
        req_time: Instant,
        event_type: String,
        json_str: String,
        rconf: RepoConfig,
    ) -> Self {
        QueueJob {
            repo_id,
            owner,
            repo,
            req_time,
            event_type,
            json_str,
            rconf,
        }
    }
}

pub(crate) struct Queue {
    q: HashMap<String, VecDeque<QueueJob>>,
}

impl Queue {
    pub fn new() -> Self {
        Queue { q: HashMap::new() }
    }

    /// Are there any jobs in the queue?
    pub fn is_empty(&self) -> bool {
        for v in self.q.values() {
            if !v.is_empty() {
                return false;
            }
        }
        true
    }

    /// Push a new request to the back of the queue.
    pub fn push_back(&mut self, qj: QueueJob) {
        let mut entry = self.q.entry(qj.repo_id.clone());

        match qj.rconf.queuekind {
            QueueKind::Evict => {
                entry = entry.and_modify(|v| v.clear());
            }
            QueueKind::Parallel | QueueKind::Sequential => (),
        }
        entry.or_default().push_back(qj);
    }

    /// Push an old request which has failed due to a temporary error back to the front of the
    /// queue so that it can be retried again on the next poll. In order that jobs are not
    /// unnecessarily pushed on the queue (which could happen with the `Evict` queue kind), the
    /// lock on `self` should be held between calls to `pop` and `push_front`.
    pub fn push_front(&mut self, qj: QueueJob) {
        self.q.entry(qj.repo_id.clone()).or_default().push_front(qj);
    }

    /// If the queue has a runnable entry, pop and return it, or `None` otherwise. Note that `None`
    /// does not guarantee that the queue is empty: it may mean that there are queued jobs that
    /// can't be run until existing jobs finish. `running(repo_id)` is a function which must return
    /// `true` if a job at `repo_id` is currently running and `false` otherwise.
    pub fn pop<F>(&mut self, running: F) -> Option<QueueJob>
    where
        F: Fn(&str) -> bool,
    {
        // We find the oldest element in the queue and pop that.
        let mut earliest_time = None;
        let mut earliest_key = None;
        for (k, v) in self.q.iter() {
            if let Some(qj) = v.front() {
                if let Some(et) = earliest_time {
                    if et > qj.req_time {
                        continue;
                    }
                }
                match qj.rconf.queuekind {
                    QueueKind::Parallel => (),
                    QueueKind::Evict | QueueKind::Sequential => {
                        if running(&qj.repo_id) {
                            continue;
                        }
                    }
                }
                earliest_time = Some(qj.req_time);
                earliest_key = Some(k.clone());
            }
        }
        // If there's an `Entry` for the key, then the corresponding value vec has at least one
        // value, so both unwrap()s are safe.
        earliest_key.map(|k| self.q.get_mut(&k).unwrap().pop_front().unwrap())
    }
}

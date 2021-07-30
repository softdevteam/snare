//! The job runner. This is a single thread which runs multiple commands as child processes. There
//! are two types of commands: "normal" and "error" commands. Error commands are only executed if a
//! normal command fails. For normal commands, we track stderr/stdout and exit status; for error
//! commands we track only exit status.

#![allow(clippy::cognitive_complexity)]

use std::{
    collections::HashMap,
    convert::TryInto,
    env,
    error::Error,
    fs::{self, remove_file},
    io::{Read, Write},
    os::unix::io::{AsRawFd, RawFd},
    path::PathBuf,
    process::{self, Child, Command},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use libc::c_int;
use nix::{
    fcntl::{fcntl, FcntlArg, OFlag},
    poll::{poll, PollFd, PollFlags},
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use tempfile::{tempdir, NamedTempFile, TempDir};

use crate::{config::RepoConfig, queue::QueueJob, Snare};

/// The size of the temporary read buffer in bytes. Should be >= PIPE_BUF for performance reasons.
const READBUF: usize = 8 * 1024;
/// Maximum time to wait in `poll` (in seconds) while waiting for child processes to terminate
/// and/or because there are jobs on the queue that we haven't been able to run yet.
const WAIT_TIMEOUT: i32 = 1;

struct JobRunner {
    snare: Arc<Snare>,
    /// The shell used to run jobs.
    shell: String,
    /// The maximum number of jobs we will run at any one point. Note that this may not necessarily
    /// be the same value as snare.conf.maxjobs.
    maxjobs: usize,
    /// The running jobs (`num_running` of which will be `Some`, the remainder `None`).
    running: Vec<Option<Job>>,
    /// How many `Some` entries are there in `self.running`?
    num_running: usize,
    /// The running jobs, with `0..2 *num_running + 1` entries. Each pair of entries are (stderr,
    /// stdout) for the corresponding `running` `Job` (i.e. `running[0]` has its stderr entry at
    /// `pollfds[0]` and stdout entry at `pollfds[1]`). The `+ 1` entry is the event file
    /// descriptor that allows the HTTP server thread to wake up the JobRunner thread.
    pollfds: Vec<PollFd>,
}

impl JobRunner {
    fn new(snare: Arc<Snare>) -> Result<Self, Box<dyn Error>> {
        let shell = env::var("SHELL")?;
        let maxjobs = snare.conf.lock().unwrap().maxjobs;
        assert!(maxjobs <= (std::usize::MAX - 1) / 2);
        let mut running = Vec::with_capacity(maxjobs);
        running.resize_with(maxjobs, || None);
        let mut pollfds = Vec::with_capacity(maxjobs * 2 + 1);
        pollfds.resize_with(maxjobs * 2 + 1, || PollFd::new(-1, PollFlags::empty()));
        Ok(JobRunner {
            snare,
            shell,
            maxjobs,
            running,
            num_running: 0,
            pollfds,
        })
    }

    /// Listen for new jobs on the queue and then run them.
    fn attend(&mut self) {
        self.update_pollfds();
        // `check_queue` serves two subtly different purposes:
        //   * Has the event pipe told us there are new jobs in the queue?
        //   * Are there jobs in the queue from a previous round that we couldn't run yet?
        let mut check_queue = false;
        // A scratch buffer used to read from files.
        let mut buf = Box::new([0; READBUF]);
        // The earliest finish_by time of any running process (i.e. the process that will timeout
        // the soonest).
        let mut next_finish_by: Option<Instant> = None;
        loop {
            // If there are jobs on the queue we haven't been able to run for temporary reasons,
            // then wait a short amount of time and try again.
            let mut timeout = if check_queue { WAIT_TIMEOUT * 1000 } else { -1 };
            // If any processes will exceed their timeout then, if that's shorter than the above
            // timeout, only wait for enough time to pass before we need to send them SIGTERM.
            if let Some(fby) = next_finish_by {
                let fby_timeout = fby.saturating_duration_since(Instant::now());
                if timeout == -1
                    || fby_timeout < Duration::from_millis(timeout.try_into().unwrap_or(0))
                {
                    timeout = fby_timeout
                        .as_millis()
                        .try_into()
                        .unwrap_or(c_int::max_value());
                }
            }
            poll(&mut self.pollfds, timeout).ok();

            self.check_for_sighup();

            // See if any of our active jobs have events. Knowing when a pipe is actually closed is
            // surprisingly hard. https://www.greenend.org.uk/rjk/tech/poll.html has an interesting
            // suggestion which we adapt slightly here.
            //
            // This `for` loop has various unwrap() calls. If `flags[i * 2]` or `flags[i * 2 + 1]`
            // is `Some(_)`, then `self.running[i]` is `Some(_)`, so the
            // `self.running.as_mut.unwrap()`s are safe. Since we asked for stderr/stdout to be
            // captured, `std[err|out].as_mut().unwrap()` should also be safe (though the Rust docs
            // are a little vague on this).
            for i in 0..self.maxjobs {
                // stderr
                if let Some(flags) = self.pollfds[i * 2].revents() {
                    if flags.contains(PollFlags::POLLIN) {
                        if let Ok(j) = self.running[i]
                            .as_mut()
                            .unwrap()
                            .child
                            .stderr
                            .as_mut()
                            .unwrap()
                            .read(&mut *buf)
                        {
                            self.running[i]
                                .as_mut()
                                .unwrap()
                                .stderrout
                                .as_file_mut()
                                .write_all(&buf[0..j])
                                .ok();
                        }
                    }
                    if flags.contains(PollFlags::POLLHUP) {
                        self.running[i].as_mut().unwrap().stderr_hup = true;
                        self.update_pollfds();
                    }
                }
                // stdout
                if let Some(flags) = self.pollfds[i * 2 + 1].revents() {
                    if flags.contains(PollFlags::POLLIN) {
                        if let Ok(j) = self.running[i]
                            .as_mut()
                            .unwrap()
                            .child
                            .stdout
                            .as_mut()
                            .unwrap()
                            .read(&mut *buf)
                        {
                            self.running[i]
                                .as_mut()
                                .unwrap()
                                .stderrout
                                .as_file_mut()
                                .write_all(&buf[0..j])
                                .ok();
                        }
                    }
                    if flags.contains(PollFlags::POLLHUP) {
                        self.running[i].as_mut().unwrap().stdout_hup = true;
                        self.update_pollfds();
                    }
                }
            }

            // Iterate over the running jobs and:
            //   * If any jobs have exceeded their timeout, send them SIGTERM.
            //   * If there are jobs whose stderr/stdout have closed, keep waiting on them until
            //     they exit.
            next_finish_by = None;
            for i in 0..self.running.len() {
                if let Some(Job {
                    finish_by,
                    ref child,
                    ..
                }) = self.running[i]
                {
                    if finish_by <= Instant::now() {
                        kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).ok();
                    } else if next_finish_by.is_none() || Some(finish_by) < next_finish_by {
                        next_finish_by = Some(finish_by);
                    }
                }

                if let Some(Job {
                    stderr_hup: true,
                    stdout_hup: true,
                    ..
                }) = self.running[i]
                {
                    // In the below, we know from the `let Some(_)` that `self.running[i]` is
                    // `Some(_)` and the unwrap thus safe.
                    let mut exited = false;
                    let mut exited_success = false;
                    match self.running[i].as_mut().unwrap().child.try_wait() {
                        Ok(Some(status)) => {
                            exited = true;
                            exited_success = status.success();
                        }
                        Err(_) => {
                            exited = true;
                            exited_success = false;
                        }
                        Ok(None) => (),
                    }
                    if exited {
                        if !exited_success {
                            let job = &self.running[i].as_ref().unwrap();
                            if job.is_errorcmd {
                                self.snare.error(&format!(
                                    "errorcmd exited unsuccessfully: {}",
                                    job.rconf.errorcmd.as_ref().unwrap()
                                ));
                            } else if let Some(errorchild) = self.run_errorcmd(job) {
                                let mut job = &mut self.running[i].as_mut().unwrap();
                                job.child = errorchild;
                                job.is_errorcmd = true;
                                continue;
                            }
                        }
                        remove_file(&self.running[i].as_ref().unwrap().json_path).ok();
                        self.running[i] = None;
                        self.num_running -= 1;
                        self.update_pollfds();
                    }
                }
            }

            // Has the HTTP server told us that we should check for new jobs and/or SIGCHLD/SIGHUP
            // has been received?
            match self.pollfds[self.maxjobs * 2].revents() {
                Some(flags) if flags == PollFlags::POLLIN => {
                    check_queue = true;
                    // It's fine for us to drain the event pipe completely: we'll process all the
                    // events it contains.
                    loop {
                        match nix::unistd::read(self.snare.event_read_fd, &mut *buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => (),
                        }
                    }
                }
                _ => (),
            }

            // Should we check the queue? This could be because we were previously unable to empty
            // it fully, or because the HTTP server has told us that there might be new jobs.
            // However, it's only worth us checking the queue (which requires a lock) if there's
            // space for us to run further jobs.
            if check_queue && self.num_running < self.maxjobs {
                check_queue = !self.try_pop_queue();
            }
        }
    }

    /// Try to pop all jobs on the queue: returns `true` if it was able to do so successfully or
    /// `false` otherwise.
    fn try_pop_queue(&mut self) -> bool {
        let snare = Arc::clone(&self.snare);
        let mut queue = snare.queue.lock().unwrap();
        loop {
            if self.num_running == self.maxjobs && !queue.is_empty() {
                return false;
            }
            let pjob = queue.pop(|repo_id| {
                self.running.iter().any(|jobslot| {
                    if let Some(job) = jobslot {
                        repo_id == job.repo_id
                    } else {
                        false
                    }
                })
            });
            match pjob {
                Some(qj) => {
                    debug_assert!(self.num_running < self.maxjobs);
                    match self.try_job(qj) {
                        Ok(j) => {
                            // The unwrap is safe since we've already checked that there's room to
                            // run at least 1 job.
                            let i = self.running.iter().position(|x| x.is_none()).unwrap();
                            self.running[i] = Some(j);
                            self.num_running += 1;
                            self.update_pollfds();
                        }
                        Err(Some(qj)) => {
                            // The job couldn't be run for temporary reasons: we'll retry later.
                            queue.push_front(qj);
                            return false;
                        }
                        Err(None) => {
                            // The job couldn't be run for permanent reasons: it has been consumed
                            // and can't be rerun. Perhaps surprisingly, this is equivalent to the
                            // job having run successfully: since it hasn't been put back on the
                            // queue, there's no need to tell the caller that we couldn't pop all
                            // the jobs on the queue.
                        }
                    }
                }
                None => {
                    // We weren't able to pop any jobs from the queue, but that doesn't mean that
                    // the queue is necessarily empty: there may be `QueueKind::Sequential` jobs in
                    // it which can't be popped until others with the same path have completed.
                    return queue.is_empty();
                }
            }
        }
    }

    /// Try starting the `QueueJob` `qj` running, returning `Ok(Job)` upon success. If for
    /// temporary reasons that is not possible, the job is returned via `Err(Some(QueueJob))` so
    /// that it can be put back in the queue and retried later. If `Err(None)` is returned then the
    /// job could not be run (either because there is no command, or because there was a permanent
    /// error, and the user was appropriately notified) and the job is consumed.
    fn try_job(&mut self, qj: QueueJob) -> Result<Job, Option<QueueJob>> {
        let raw_cmd = match &qj.rconf.cmd {
            Some(c) => c,
            None => {
                // There is no command to run.
                return Err(None);
            }
        };

        // Write the JSON to an unnamed temporary file.
        let json_path = match NamedTempFile::new() {
            Ok(tfile) => match tfile.into_temp_path().keep() {
                Ok(p) => {
                    if let Err(e) = fs::write(&p, qj.json_str.as_bytes()) {
                        self.snare.error_err("Couldn't write JSON file.", e);
                        remove_file(p).ok();
                        return Err(Some(qj));
                    }
                    p
                }
                Err(e) => {
                    self.snare.error_err("Couldn't create temporary file.", e);
                    return Err(Some(qj));
                }
            },
            Err(e) => {
                self.snare.error_err("Couldn't create temporary file.", e);
                return Err(Some(qj));
            }
        };

        // We combine the child process's stderr/stdout and write them to an unnamed temporary
        // file `stderrout_file`.
        if let Ok(tempdir) = tempdir() {
            if let Ok(stderrout) = NamedTempFile::new() {
                if set_nonblock(stderrout.as_file().as_raw_fd()).is_ok() {
                    if let Some(json_path_str) = json_path.to_str() {
                        let cmd = cmd_replace(
                            raw_cmd,
                            &qj.event_type,
                            &qj.owner,
                            &qj.repo,
                            json_path_str,
                        );
                        let child = match Command::new(&self.shell)
                            .arg("-c")
                            .arg(cmd)
                            .current_dir(tempdir.path())
                            .stderr(process::Stdio::piped())
                            .stdout(process::Stdio::piped())
                            .stdin(process::Stdio::null())
                            .spawn()
                        {
                            Ok(c) => c,
                            Err(e) => {
                                self.snare.error_err("Can't spawn command: {:?}", e);
                                return Err(None);
                            }
                        };

                        // Since we've asked for stderr/stdout to be captured, the unwrap()s should
                        // be safe, though the Rust docs are slightly vague on this.
                        let stderr = child.stderr.as_ref().unwrap();
                        let stdout = child.stdout.as_ref().unwrap();

                        let stderr_fd = stderr.as_raw_fd();
                        let stdout_fd = stdout.as_raw_fd();
                        if let Err(e) =
                            set_nonblock(stderr_fd).and_then(|_| set_nonblock(stdout_fd))
                        {
                            self.snare
                                .error_err("Can't set file descriptors to non-blocking: {:?}", e);
                            return Err(None);
                        }

                        // This unwrap() is, in theory, unsafe because we could exceed the timeout
                        // duration. However, a quick back-of-the-envelope calculation suggests
                        // that, assuming `Instant` is a `u64`, this could only happen with an
                        // uptime of over 500,000,000 years. This seems adequately long that I'm
                        // happy to take the risk on the unwrap().
                        let finish_by = Instant::now()
                            .checked_add(Duration::from_millis(
                                qj.rconf.timeout.saturating_mul(1000),
                            ))
                            .unwrap();

                        return Ok(Job {
                            is_errorcmd: false,
                            repo_id: qj.repo_id,
                            event_type: qj.event_type,
                            owner: qj.owner,
                            repo: qj.repo,
                            finish_by,
                            child,
                            tempdir,
                            json_path,
                            stderrout,
                            stderr_hup: false,
                            stdout_hup: false,
                            rconf: qj.rconf,
                        });
                    }
                }
            }
        }

        Err(Some(qj))
    }

    /// After a job has been inserted / removed from `self.running`, this function must be called
    /// so that `poll()` is called with up-to-date file descriptors.
    fn update_pollfds(&mut self) {
        for (i, jobslot) in self.running.iter().enumerate() {
            let (stderr_fd, stdout_fd) = if let Some(job) = jobslot {
                // Since we've asked for stderr/stdout to be captured, the unwrap()s should be
                // safe, though the Rust docs are slightly vague on this.
                let stderr_fd = if job.stderr_hup {
                    -1
                } else {
                    job.child.stderr.as_ref().unwrap().as_raw_fd()
                };
                let stdout_fd = if job.stdout_hup {
                    -1
                } else {
                    job.child.stdout.as_ref().unwrap().as_raw_fd()
                };
                (stderr_fd, stdout_fd)
            } else {
                (-1, -1)
            };
            self.pollfds[i * 2] = PollFd::new(stderr_fd, PollFlags::POLLIN);
            self.pollfds[i * 2 + 1] = PollFd::new(stdout_fd, PollFlags::POLLIN);
        }
        self.pollfds[self.maxjobs * 2] = PollFd::new(self.snare.event_read_fd, PollFlags::POLLIN);
    }

    /// If SIGHUP has been received, reload the config, and update self.maxjobs if possible.
    fn check_for_sighup(&mut self) {
        self.snare.check_for_sighup();

        let new_maxjobs = self.snare.conf.lock().unwrap().maxjobs;
        if new_maxjobs > self.maxjobs {
            // The user now wants to allow more jobs which we can do simply and safely -- even if
            // there are jobs running -- by extending self.running and self.pollfds with blank
            // entries.
            self.running.resize_with(new_maxjobs, || None);
            self.pollfds
                .resize_with(new_maxjobs * 2 + 1, || PollFd::new(-1, PollFlags::empty()));
            self.maxjobs = new_maxjobs;
            self.update_pollfds();
        } else if new_maxjobs < self.maxjobs && self.num_running == 0 {
            // The user wants to allow fewer jobs. This is somewhat hard because we may be running
            // jobs, and possibly more than the user now wants us to be running. We could be clever
            // and compact self.running and self.pollfds, though that may still not drop the number
            // of jobs down enough. We currently do the laziest thing: we wait until there are no
            // running jobs and then truncate self.running and self.pollfds. If there are always
            // running jobs then this means we will never reduce the number of maximum possible
            // jobs.
            self.running.truncate(new_maxjobs);
            self.pollfds.truncate(new_maxjobs * 2 + 1);
            self.maxjobs = new_maxjobs;
            self.update_pollfds();
        }
    }

    /// If the user has specified an email address, send the contents of
    fn run_errorcmd(&self, job: &Job) -> Option<Child> {
        if let Some(raw_errorcmd) = &job.rconf.errorcmd {
            let errorcmd = errorcmd_replace(
                raw_errorcmd,
                &job.event_type,
                &job.owner,
                &job.repo,
                job.json_path.as_os_str().to_str().unwrap(),
                job.stderrout.path().as_os_str().to_str().unwrap(),
            );
            match Command::new(&self.shell)
                .arg("-c")
                .arg(&errorcmd)
                .current_dir(job.tempdir.path())
                .stderr(process::Stdio::null())
                .stdout(process::Stdio::null())
                .stdin(process::Stdio::null())
                .spawn()
            {
                Ok(c) => return Some(c),
                Err(e) => self
                    .snare
                    .error_err(&format!("Can't spawn '{}'", errorcmd), e),
            }
        }
        None
    }
}

/// Take the string `raw_cmd` and return a string with the following replaced:
///   * `%e` with `event_type`
///   * `%o` with `owner`
///   * `%r` with `repo`
///   * `%j` with `json_path`
///
/// Note that `raw_cmd` *must* have been validated by config::GitHub::verify_cmd_str or undefined
/// behaviour will occur.
fn cmd_replace(
    raw_cmd: &str,
    event_type: &str,
    owner: &str,
    repo: &str,
    json_path: &str,
) -> String {
    let modifiers = [
        ('e', event_type),
        ('o', owner),
        ('r', repo),
        ('j', json_path),
        ('%', "%"),
    ]
    .iter()
    .cloned()
    .collect();
    replace(raw_cmd, modifiers)
}

/// Take the string `raw_errorcmd` and return a string with the following replaced:
///   * `%e` with `event_type`
///   * `%o` with `owner`
///   * `%r` with `repo`
///   * `%j` with `json_path`
///   * `%s` with `stderrout_path`
///
/// Note that `raw_cmd` *must* have been validated by config::GitHub::verify_errorcmd_str or
/// undefined behaviour will occur.
fn errorcmd_replace(
    raw_errorcmd: &str,
    event_type: &str,
    owner: &str,
    repo: &str,
    json_path: &str,
    stderrout_path: &str,
) -> String {
    let modifiers = [
        ('e', event_type),
        ('o', owner),
        ('r', repo),
        ('j', json_path),
        ('s', stderrout_path),
        ('%', "%"),
    ]
    .iter()
    .cloned()
    .collect();
    replace(raw_errorcmd, modifiers)
}

fn replace(s: &str, modifiers: HashMap<char, &str>) -> String {
    // Except in the presence of '%%'s, the output string will be at least as long as the input
    // string, so starting at that capacity is a reasonable heuristic.
    let mut n = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with('%') {
            let mdf = s[i + 1..].chars().next().unwrap(); // modifier
            n.push_str(modifiers.get(&mdf).unwrap());
            i += 1 + mdf.len_utf8();
        } else {
            let c = s[i..].chars().next().unwrap();
            n.push(c);
            i += c.len_utf8();
        }
    }
    n
}

struct Job {
    /// Set to `false` if this is a normal command and `true` if it is an error command.
    is_errorcmd: bool,
    /// The repo identifier. This is used to determine if a given repository already has jobs
    /// running or not. Typically of the form "provider/owner/repo".
    repo_id: String,
    /// The event type.
    event_type: String,
    /// The repository owner's name.
    owner: String,
    /// The repository name.
    repo: String,
    /// What time must this Job have completed by? If it exceeds this time, it will be terminated.
    finish_by: Instant,
    /// The child process itself.
    child: Child,
    /// This TempDir will be dropped, and its file system contents removed, when this Job is dropped.
    tempdir: TempDir,
    /// We are responsible for manually cleaning up the JSON file stored in `json_path`.
    json_path: PathBuf,
    /// The temporary file to which we write combined stderr/stdout.
    stderrout: NamedTempFile,
    /// Has the child process's stderr been closed?
    stderr_hup: bool,
    /// Has the child process's stdout been closed?
    stdout_hup: bool,
    /// The `RepoConfig` for this job.
    rconf: RepoConfig,
}

fn set_nonblock(fd: RawFd) -> Result<(), Box<dyn Error>> {
    let mut flags = fcntl(fd, FcntlArg::F_GETFL)?;
    flags |= OFlag::O_NONBLOCK.bits();
    fcntl(
        fd,
        FcntlArg::F_SETFL(unsafe { OFlag::from_bits_unchecked(flags) }),
    )?;
    Ok(())
}

pub(crate) fn attend(snare: Arc<Snare>) -> Result<(), Box<dyn Error>> {
    let mut rn = JobRunner::new(snare)?;
    thread::spawn(move || rn.attend());
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_cmd_replace() {
        assert_eq!(cmd_replace("", "", "", "", ""), "");
        assert_eq!(cmd_replace("a", "", "", "", ""), "a");
        assert_eq!(
            cmd_replace("%% %e %o %r %j %%", "ee", "oo", "rr", "jj"),
            "% ee oo rr jj %"
        );
    }

    #[test]
    fn test_errorcmd_replace() {
        assert_eq!(errorcmd_replace("", "", "", "", "", ""), "");
        assert_eq!(errorcmd_replace("a", "", "", "", "", ""), "a");
        assert_eq!(
            errorcmd_replace("%% %e %o %r %j %s %%", "ee", "oo", "rr", "jj", "ss"),
            "% ee oo rr jj ss %"
        );
    }
}

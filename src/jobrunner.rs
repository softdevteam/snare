use std::{
    error::Error,
    fs::{self, remove_file, File},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::io::{AsRawFd, RawFd},
    path::PathBuf,
    process::{self, Child, Command},
    sync::Arc,
    thread,
};

use lettre::{sendmail::SendmailTransport, EmailAddress, Envelope, SendableEmail, Transport};
use nix::{
    fcntl::{fcntl, FcntlArg, OFlag},
    poll::{poll, PollFd, PollFlags},
};
use tempfile::{tempdir, tempfile, NamedTempFile, TempDir};
use whoami::{hostname, username};

use crate::{config::RepoConfig, queue::QueueJob, Snare};

/// The size of the temporary read buffer in bytes. Should be >= PIPE_BUF for performance reasons.
const READBUF: usize = 8 * 1024;
/// Maximum time to wait in `poll` (in seconds) while waiting for child processes to terminate
/// and/or because there are jobs on the queue that we haven't been able to run yet.
const WAIT_TIMEOUT: i32 = 1;

struct JobRunner {
    snare: Arc<Snare>,
    /// The maximum number of jobs we will run at any one point. Note that this may not necessarily
    /// be the same value as snare.config.maxjobs.
    maxjobs: usize,
    /// The running jobs, with `0..num_running` entries.
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
        // If the unwrap() on the lock fails, the other thread has paniced.
        let maxjobs = snare.config.lock().unwrap().maxjobs;
        assert!(maxjobs < std::usize::MAX);
        let mut running = Vec::with_capacity(maxjobs);
        running.resize_with(maxjobs, || None);
        let mut pollfds = Vec::with_capacity(maxjobs * 2 + 1);
        pollfds.resize_with(maxjobs * 2 + 1, || PollFd::new(-1, PollFlags::empty()));
        Ok(JobRunner {
            snare,
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
        // How many jobs have closed stderr/stdout and which we're just waiting to terminate?
        let mut num_waiting = 0;
        // A scratch buffer used to read from files.
        let mut buf = Box::new([0; READBUF]);
        loop {
            // If we're waiting for jobs to die or if there are jobs on the queue we haven't been
            // able to run for temporary reasons, then wait a short amount of time and try again.
            // Notice that the second clause is a bit subtle: if there are jobs on the queue, but
            // we're all running the maximum number of jobs, then there's no point in waking up.
            let timeout = if num_waiting > 0 || (check_queue && self.num_running < self.maxjobs) {
                WAIT_TIMEOUT * 1000
            } else {
                -1
            };
            poll(&mut self.pollfds, timeout).ok();

            // The SIGHUP listener writes to the event FD if SIGHUP has been received, so it's
            // possible that the reason we've been woken up is that the SIGHUP event needs to be
            // processed.
            self.snare.check_for_hup();

            // See if any of our active jobs have events. Knowing when a pipe is actually closed is
            // surprisingly hard. https://www.greenend.org.uk/rjk/tech/poll.html has an interesting
            // suggestion which we adapt slightly here.
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
                                .stderrout_file
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
                                .stderrout_file
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

            // If there are jobs whose stderr/stdout have closed, keep waiting on them until they
            // exit.
            num_waiting = 0;
            for i in 0..self.running.len() {
                if let Some(Job {
                    stderr_hup: true,
                    stdout_hup: true,
                    ..
                }) = self.running[i]
                {
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
                            self.sendemail(&job.rconf.email, &job.stderrout_file);
                        }
                        remove_file(&self.running[i].as_ref().unwrap().json_path).ok();
                        self.running[i] = None;
                        self.num_running -= 1;
                        self.update_pollfds();
                    } else {
                        num_waiting += 1;
                    }
                }
            }

            // Has the HTTP server told us that it's put more jobs into the queue?
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
            // it fully, or because the HTTP server has told us that it's put more jobs there.
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
        // Note that the various `unwrap` calls to `queue.lock()` are acceptable because if if it
        // fails it means that something has gone so seriously wrong in the other thread that
        // there's no likelihood that we can recover.

        loop {
            let pjob = self.snare.queue.lock().unwrap().pop();
            match pjob {
                Some(qj) => {
                    if self.num_running == self.maxjobs {
                        self.snare.queue.lock().unwrap().push_front(qj);
                        return false;
                    }
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
                            self.snare.queue.lock().unwrap().push_front(qj);
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
                None => return true,
            }
        }
    }

    /// Try starting the `QueueJob` `qj` running, returning `Ok(Job)` upon success. If for
    /// temporary reasons that is not possible, the job is returned via `Err(Some(QueueJob))` so
    /// that it can be put back in the queue and retried later. If `Err(None)` is returned then the
    /// job could not be run for permanent reasons and the job is consumed.
    fn try_job(&mut self, qj: QueueJob) -> Result<Job, Option<QueueJob>> {
        // Write the JSON to an unnamed temporary file.
        let json_path = match NamedTempFile::new() {
            Ok(tfile) => match tfile.into_temp_path().keep() {
                Ok(p) => {
                    if fs::write(&p, qj.json_str.as_bytes()).is_err() {
                        remove_file(p).ok();
                        return Err(Some(qj));
                    }
                    p
                }
                Err(_) => return Err(Some(qj)),
            },
            Err(_) => return Err(Some(qj)),
        };

        // We combine the child process's stderr/stdout and write them to an unnamed temporary
        // file `stderrout_file`.
        if let Ok(tempdir) = tempdir() {
            if let Ok(stderrout_file) = tempfile() {
                if set_nonblock(stderrout_file.as_raw_fd()).is_ok() {
                    if let Some(json_path_str) = json_path.to_str() {
                        let child = match Command::new(qj.path)
                            .arg(qj.event_type)
                            .arg(json_path_str)
                            .current_dir(tempdir.path())
                            .stderr(process::Stdio::piped())
                            .stdout(process::Stdio::piped())
                            .stdin(process::Stdio::null())
                            .spawn()
                        {
                            Ok(c) => c,
                            Err(e) => {
                                self.warning(&format!("Can't spawn command: {:?}", e));
                                return Err(None);
                            }
                        };

                        let stderr = child.stderr.as_ref().unwrap();
                        let stdout = child.stdout.as_ref().unwrap();

                        let stderr_fd = stderr.as_raw_fd();
                        let stdout_fd = stdout.as_raw_fd();
                        if let Err(e) =
                            set_nonblock(stderr_fd).and_then(|_| set_nonblock(stdout_fd))
                        {
                            self.warning(&format!(
                                "Can't set file descriptors to non-blocking: {:?}",
                                e
                            ));
                            return Err(None);
                        }

                        return Ok(Job {
                            child,
                            _tempdir: tempdir,
                            json_path,
                            stderrout_file,
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

    /// If the user has specified an email address, send the contents of
    fn sendemail(&self, email: &Option<String>, mut file: &File) {
        if let Some(ref toaddr) = email {
            let mut buf = Vec::new();
            buf.extend("Subject: snare error\n\n".as_bytes());
            file.seek(SeekFrom::Start(0)).ok();
            file.read_to_end(&mut buf).ok();

            let fromea = match EmailAddress::new(format!("{}@{}", username(), hostname())) {
                Ok(ea) => Some(ea),
                Err(_) => None,
            };

            match EmailAddress::new(toaddr.to_string()) {
                Ok(toea) => {
                    let email = SendableEmail::new(
                        Envelope::new(fromea, vec![toea]).unwrap(),
                        "na".to_string(),
                        buf,
                    );

                    let mut sender = SendmailTransport::new();
                    if let Err(e) = sender.send(email) {
                        self.warning(&format!("Couldn't send email: {:?}", e));
                    }
                }
                Err(_) => self.warning(&format!("Invalid To: email address {}", toaddr)),
            }
        }
    }

    /// Log a warning to the user. This might be to stderr, a log file, or email.
    fn warning(&self, msg: &str) {
        eprintln!("Warning: {}\n", msg);
    }
}

struct Job {
    child: Child,
    /// This TempDir will be dropped, and its file system contents removed, when this Job is dropped.
    _tempdir: TempDir,
    /// We are responsible for manually cleaning up the JSON file stored in `json_path`.
    json_path: PathBuf,
    /// The file to which we write combined stderr/stdout.
    stderrout_file: File,
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

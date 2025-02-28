//! snare is a GitHub webhooks daemon. Architecturally it is split in two:
//!   * The `httpserver` listens for incoming hooks, checks that they're valid, and adds them to a
//!     `Queue`.
//!   * The `jobrunner` pops elements from the `Queue` and runs them in parallel.
//!
//! These two components run as two different threads: the `httpserver` writes a solitary byte to
//! an "event pipe" to wake up the `jobrunner` when the queue has new elements. We also wake up the
//! `jobrunner` on SIGHUP and SIGCHLD.

#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod config;
mod config_ast;
mod httpserver;
mod jobrunner;
mod queue;

use std::{
    env::{self, current_exe, set_current_dir},
    ffi::CString,
    os::unix::io::RawFd,
    path::PathBuf,
    process,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use getopts::Options;
use libc::{
    c_char, openlog, syslog, LOG_CONS, LOG_CRIT, LOG_DAEMON, LOG_ERR, LOG_INFO, LOG_WARNING,
};
use nix::{
    fcntl::OFlag,
    unistd::{daemon, pipe2, setresgid, setresuid, Gid, Uid},
};
use pwd::Passwd;

use config::Config;
use queue::Queue;

/// Default location of `snare.conf`.
const SNARE_CONF_PATH: &str = "/etc/snare/snare.conf";

#[repr(u8)]
#[derive(PartialEq, PartialOrd)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
}

pub(crate) struct Snare {
    /// Are we currently running as a daemon?
    daemonised: bool,
    /// What level do we want to log at?
    log_level: LogLevel,
    /// The location of snare.conf; this file will be reloaded if SIGHUP is received.
    conf_path: PathBuf,
    /// The current configuration: note that this can change at any point due to SIGHUP. All calls
    /// to `conf.lock().unwrap()` are considered safe since the only way this can fail is if the
    /// other thread has `panic`ed, at which point we're already doomed.
    conf: Mutex<Config>,
    /// The current queue of incoming jobs. All calls to `queue.lock().unwrap()` are considered
    /// safe since the only way this can fail is if the other thread has `panic`ed, at which point
    /// we're already doomed.
    queue: Mutex<Queue>,
    /// The read end of the pipe used by the httpserver and the SIGHUP handler to wake up the job
    /// runner thread.
    event_read_fd: RawFd,
    /// The write end of the pipe used by the httpserver and the SIGHUP handler to wake up the job
    /// runner thread.
    event_write_fd: RawFd,
    /// Has a SIGHUP event occurred? If so, the jobrunner will process it, and set this to false in
    /// case future SIGHUP events are detected.
    sighup_occurred: Arc<AtomicBool>,
}

impl Snare {
    /// Check to see if we've received a SIGHUP since the last check. If so, we will try reloading
    /// the snare.conf file specified when we started. **Note that another thread may have called
    /// this function and caused the config to have changed.**
    fn check_for_sighup(&self) {
        if self.sighup_occurred.load(Ordering::Relaxed) {
            match Config::from_path(&self.conf_path) {
                Ok(conf) => *self.conf.lock().unwrap() = conf,
                Err(msg) => self.error(&msg),
            }
            self.sighup_occurred.store(false, Ordering::Relaxed);
        }
    }

    fn log(&self, msg: &str, log_level: LogLevel) {
        if log_level > self.log_level {
            return;
        }
        if self.daemonised {
            // We know that `%s` and `<can't represent as CString>` are both valid C strings, and
            // that neither unwrap() can fail.
            let fmt = CString::new("%s").unwrap();
            let msg = CString::new(msg)
                .unwrap_or_else(|_| CString::new("<can't represent as CString>").unwrap());
            let syslog_level = match self.log_level {
                LogLevel::Error => LOG_ERR,
                LogLevel::Warn => LOG_WARNING,
                LogLevel::Info => LOG_INFO,
            };
            unsafe {
                syslog(syslog_level, fmt.as_ptr(), msg.as_ptr());
            }
        } else {
            eprintln!("{}", msg);
        }
    }

    /// Log `msg` as an error.
    ///
    /// # Panics
    ///
    /// If `msg` contains a `NUL` byte.
    pub fn error(&self, msg: &str) {
        self.log(msg, LogLevel::Error);
    }

    /// Log `msg` as a warning.
    ///
    /// # Panics
    ///
    /// If `msg` contains a `NUL` byte.
    pub fn warn(&self, msg: &str) {
        self.log(msg, LogLevel::Warn);
    }

    /// Log `msg` as an informational message.
    ///
    /// # Panics
    ///
    /// If `msg` contains a `NUL` byte.
    pub fn info(&self, msg: &str) {
        self.log(msg, LogLevel::Info);
    }
}

/// Try to find a `snare.conf` file.
fn search_snare_conf() -> Option<PathBuf> {
    let p = PathBuf::from(SNARE_CONF_PATH);
    if p.is_file() {
        return Some(p);
    }
    None
}

fn user_from_name(n: &str) -> Option<Passwd> {
    match Passwd::from_name(n) {
        Ok(Some(x)) => Some(x),
        Ok(None) => None,
        Err(e) => fatal(
            false,
            &format!("Can't access user information for {n}: {e}"),
        ),
    }
}

/// If the config specified a 'user' then switch to that and update $HOME and $USER appropriately.
/// This function must not be called after daemonisation.
fn change_user(conf: &Config) {
    match conf.user {
        Some(ref user) => match user_from_name(user) {
            Some(u) => {
                let gid = Gid::from_raw(u.gid);
                if let Err(e) = setresgid(gid, gid, gid) {
                    fatal(false, &format!("Can't switch to group '{user}': {e}"))
                }
                let uid = Uid::from_raw(u.uid);
                if let Err(e) = setresuid(uid, uid, uid) {
                    fatal(false, &format!("Can't switch to user '{user}': {e}"))
                }
                env::set_var("HOME", u.dir);
                env::set_var("USER", user);
            }
            None => fatal(false, &format!("Unknown user '{user}'")),
        },
        None => {
            if Uid::current().is_root() {
                fatal(
                    false,
                    "The 'user' option must be set if snare is run as root",
                );
            }
        }
    }
}

fn progname() -> String {
    match current_exe() {
        Ok(p) => p
            .file_name()
            .map(|x| x.to_str().unwrap_or("snare"))
            .unwrap_or("snare")
            .to_owned(),
        Err(_) => "snare".to_owned(),
    }
}

/// Exit with a fatal error.
fn fatal(daemonised: bool, msg: &str) -> ! {
    if daemonised {
        // We know that `%s` and `<can't represent as CString>` are both valid C strings, and
        // that neither unwrap() can fail.
        let fmt = CString::new("%s").unwrap();
        let msg = CString::new(msg)
            .unwrap_or_else(|_| CString::new("<can't represent as CString>").unwrap());
        unsafe {
            syslog(LOG_CRIT, fmt.as_ptr(), msg.as_ptr());
        }
    } else {
        eprintln!("{}", msg);
    }
    process::exit(1);
}

/// Print out program usage then exit. This function must not be called after daemonisation.
fn usage() -> ! {
    eprintln!("Usage: {} [-c <config-path>] [-d]", progname());
    process::exit(1)
}

pub fn main() {
    let args: Vec<String> = env::args().collect();
    let matches = Options::new()
        .optmulti("c", "config", "Path to snare.conf.", "<conf-path>")
        .optflag(
            "d",
            "",
            "Don't detach from the terminal and log errors to stderr.",
        )
        .optflag("h", "help", "")
        .optflagmulti("v", "verbose", "")
        .parse(&args[1..])
        .unwrap_or_else(|_| usage());
    if matches.opt_present("h") {
        usage();
    }

    let daemonise = !matches.opt_present("d");

    let conf_path = match matches.opt_str("c") {
        Some(p) => PathBuf::from(&p),
        None => search_snare_conf().unwrap_or_else(|| fatal(false, "Can't find snare.conf")),
    };
    let conf = Config::from_path(&conf_path).unwrap_or_else(|m| fatal(false, &m));

    let log_level = match matches.opt_count("v") {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        _ => LogLevel::Info,
    };

    change_user(&conf);

    set_current_dir("/").unwrap_or_else(|_| fatal(false, "Can't chdir to '/'"));
    if daemonise {
        if let Err(e) = daemon(true, false) {
            fatal(false, &format!("Couldn't daemonise: {e}"));
        }
    }

    // openlog's first argument `ident` is incompletely specified, but in practise we have to
    // assume that syslog merely stores a pointer to the string (i.e. it doesn't copy the string).
    // We thus deliberately leak memory here in order that the pointer always points to valid
    // memory. The unwrap() here is ugly, but if it fails, it means we've run out of memory, so
    // it's neither likely to fail nor, if it does, can we do anything to clear up from it.
    let progname =
        Box::into_raw(CString::new(progname()).unwrap().into_boxed_c_str()) as *const c_char;
    unsafe {
        openlog(progname, LOG_CONS, LOG_DAEMON);
    }

    let (event_read_fd, event_write_fd) = match pipe2(OFlag::O_NONBLOCK) {
        Ok(p) => p,
        Err(e) => {
            fatal(false, &format!("Can't create pipe: {e}"));
        }
    };
    let sighup_occurred = Arc::new(AtomicBool::new(false));
    {
        let sighup_occurred = Arc::clone(&sighup_occurred);
        if let Err(e) = unsafe {
            signal_hook::low_level::register(signal_hook::consts::SIGHUP, move || {
                // All functions called in this function must be signal safe. See signal(3).
                sighup_occurred.store(true, Ordering::Relaxed);
                nix::unistd::write(event_write_fd, &[0]).ok();
            })
        } {
            fatal(daemonise, &format!("Can't install SIGHUP handler: {e}"));
        }
        if let Err(e) = unsafe {
            signal_hook::low_level::register(signal_hook::consts::SIGCHLD, move || {
                // All functions called in this function must be signal safe. See signal(3).
                nix::unistd::write(event_write_fd, &[0]).ok();
            })
        } {
            fatal(daemonise, &format!("Can't install SIGCHLD handler: {e}"));
        }
    }

    let snare = Arc::new(Snare {
        daemonised: daemonise,
        log_level,
        conf_path,
        conf: Mutex::new(conf),
        queue: Mutex::new(Queue::new()),
        event_read_fd,
        event_write_fd,
        sighup_occurred,
    });

    match jobrunner::attend(Arc::clone(&snare)) {
        Ok(x) => x,
        Err(e) => {
            fatal(daemonise, &format!("Couldn't start runner thread: {e}"));
        }
    }

    httpserver::serve(snare).unwrap();
}

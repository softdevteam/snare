//! snare is a GitHub webhooks daemon. Architecturally it is split in two:
//!   * The `httpserver` listens for incoming hooks, checks that they're valid, and adds them to a
//!     `Queue`.
//!   * The `jobrunner` pops elements from the `Queue` and runs them in parallel.
//! These two components run as two different threads: the `httpserver` writes a solitary byte to
//! an "event pipe" to wake up the `jobrunner` when the queue has new elements. We also wake up the
//! `jobrunner` on SIGHUP and SIGCHLD.

#![allow(clippy::type_complexity)]

mod config;
mod config_ast;
mod httpserver;
mod jobrunner;
mod queue;

use std::{
    env::{self, current_exe, set_current_dir},
    os::unix::io::RawFd,
    path::PathBuf,
    process,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use getopts::Options;
use log::error;
use nix::{
    fcntl::OFlag,
    unistd::{daemon, pipe2, setresgid, setresuid, Gid, Uid},
};
use pwd::Passwd;

use config::Config;
use queue::Queue;

/// Default location of `snare.conf`.
const SNARE_CONF_PATH: &str = "/etc/snare/snare.conf";

pub(crate) struct Snare {
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
                Err(msg) => error!("Couldn't reload config: {msg}"),
            }
            self.sighup_occurred.store(false, Ordering::Relaxed);
        }
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
        Err(e) => fatal(&format!("Can't access user information for {n}: {e}")),
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
                    fatal(&format!("Can't switch to group '{user}': {e}"))
                }
                let uid = Uid::from_raw(u.uid);
                if let Err(e) = setresuid(uid, uid, uid) {
                    fatal(&format!("Can't switch to user '{user}': {e}"))
                }
                env::set_var("HOME", u.dir);
                env::set_var("USER", user);
            }
            None => fatal(&format!("Unknown user '{user}'")),
        },
        None => {
            if Uid::current().is_root() {
                fatal("The 'user' option must be set if snare is run as root");
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
fn fatal(msg: &str) -> ! {
    eprintln!("{msg:}");
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
        None => search_snare_conf().unwrap_or_else(|| fatal("Can't find snare.conf")),
    };
    let conf = Config::from_path(&conf_path).unwrap_or_else(|m| fatal(&m));

    change_user(&conf);

    set_current_dir("/").unwrap_or_else(|_| fatal("Can't chdir to '/'"));
    if daemonise {
        let formatter = syslog::Formatter3164 {
            process: progname(),
            ..Default::default()
        };
        let logger = syslog::unix(formatter)
            .unwrap_or_else(|e| fatal(&format!("Cannot connect to syslog: {e:}")));
        let levelfilter = match matches.opt_count("v") {
            0 => log::LevelFilter::Error,
            1 => log::LevelFilter::Warn,
            2 => log::LevelFilter::Info,
            3 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };
        log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
            .map(|()| log::set_max_level(levelfilter))
            .unwrap_or_else(|e| fatal(&format!("Cannot set logger: {e:}")));
        if let Err(e) = daemon(true, false) {
            fatal(&format!("Couldn't daemonise: {e}"));
        }
    } else {
        stderrlog::new()
            .module(module_path!())
            .verbosity(matches.opt_count("v"))
            .init()
            .unwrap();
    }

    let (event_read_fd, event_write_fd) = match pipe2(OFlag::O_NONBLOCK) {
        Ok(p) => p,
        Err(e) => {
            error!("Can't create pipe: {e}");
            process::exit(1);
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
            error!("Can't install SIGHUP handler: {e}");
            process::exit(1);
        }
        if let Err(e) = unsafe {
            signal_hook::low_level::register(signal_hook::consts::SIGCHLD, move || {
                // All functions called in this function must be signal safe. See signal(3).
                nix::unistd::write(event_write_fd, &[0]).ok();
            })
        } {
            error!("Can't install SIGCHLD handler: {e}");
            process::exit(1);
        }
    }

    let snare = Arc::new(Snare {
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
            error!("Couldn't start runner thread: {e}");
            process::exit(1);
        }
    }

    httpserver::serve(snare).unwrap();
}

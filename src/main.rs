//! snare is a GitHub webhooks runner. Architecturally it is split in two:
//!   * The `httpserver` listens for incoming hooks, checks that they're valid, and adds them to a
//!     `Queue`.
//!   * The `jobrunner` pops elements from the `Queue` and runs them in parallel.
//! These two components run as two different threads: the `httpserver` writes a solitary byte to
//! an "event pipe" to wake up the `jobrunner` when the queue has new elements. This allows the
//! `jobrunner` to use a single interface for listen for completed jobs as well as new jobs.

mod config;
mod config_ast;
mod httpserver;
mod jobrunner;
mod queue;

use std::{
    env,
    error::Error,
    fmt::Display,
    io::{stderr, Write},
    net::SocketAddr,
    os::unix::io::RawFd,
    path::{Path, PathBuf},
    process,
    sync::{Arc, Mutex},
};

use getopts::Options;
use hyper::Server;
use nix::{fcntl::OFlag, unistd::pipe2};
use users::{get_current_uid, get_user_by_uid, os::unix::UserExt};

use config::Config;
use queue::Queue;

/// Default locations to look for `snare.conf`: `~/` will be automatically converted to the current
/// user's home directory.
const SNARE_CONF_SEARCH: &[&str] = &["/etc/snare.conf", "~/.snare.conf"];

pub(crate) struct Snare {
    config: Mutex<Config>,
    queue: Mutex<Queue>,
    event_read_fd: RawFd,
    event_write_fd: RawFd,
}

/// Exit with a fatal error.
fn fatal(msg: &str) -> ! {
    debug_assert!(msg.ends_with('.'));
    eprintln!("{}.", msg);
    process::exit(1);
}

/// Exit with a fatal error, printing the contents of `err`.
fn fatal_err<E: Into<Box<dyn Error>> + Display>(msg: &str, err: E) -> ! {
    fatal(&format!("{}: {}", msg, err));
}

/// Try to find a `snare.conf` file.
fn search_snare_conf() -> Option<PathBuf> {
    for cnd in SNARE_CONF_SEARCH {
        if cnd.starts_with("~/") {
            let mut homedir =
                match get_user_by_uid(get_current_uid()).map(|x| x.home_dir().to_path_buf()) {
                    Some(p) => p,
                    None => continue,
                };
            homedir.push(&cnd[2..]);
            if homedir.is_file() {
                return Some(homedir);
            }
        }
        let p = PathBuf::from(cnd);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Print out program usage then exit.
fn usage(prog: &str) -> ! {
    let path = Path::new(prog);
    let leaf = path
        .file_name()
        .map(|x| x.to_str().unwrap_or("snare"))
        .unwrap_or("snare");
    writeln!(
        &mut stderr(),
        "Usage: {} [-e email] [-j <max-jobs>] -p <port> -r <repos-dir> -s <secrets-path>",
        leaf
    )
    .ok();
    process::exit(1)
}

#[tokio::main]
pub async fn main() {
    let args: Vec<String> = env::args().collect();
    let prog = &args[0];
    let matches = Options::new()
        .optmulti("c", "config", "Path to snare.conf.", "<path>")
        .optflag("h", "help", "")
        .parse(&args[1..])
        .unwrap_or_else(|_| usage(prog));
    if matches.opt_present("h") {
        usage(prog);
    }

    let conf_path = match matches.opt_str("c") {
        Some(p) => PathBuf::from(&p),
        None => search_snare_conf().unwrap_or_else(|| fatal("Can't find snare.conf")),
    };
    let config = Config::from_path(conf_path);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let server = match Server::try_bind(&addr) {
        Ok(s) => s,
        Err(e) => fatal_err("Couldn't bind to port", e),
    };

    let (event_read_fd, event_write_fd) = pipe2(OFlag::O_NONBLOCK).unwrap();
    let snare = Arc::new(Snare {
        config: Mutex::new(config),
        queue: Mutex::new(Queue::new()),
        event_read_fd,
        event_write_fd,
    });

    match jobrunner::attend(Arc::clone(&snare)) {
        Ok(x) => x,
        Err(e) => fatal_err("Couldn't start runner thread", e),
    }

    httpserver::serve(server, Arc::clone(&snare)).await;
}

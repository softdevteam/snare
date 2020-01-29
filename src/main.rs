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
    error::Error,
    fmt::Display,
    net::SocketAddr,
    os::unix::io::RawFd,
    process,
    sync::{Arc, Mutex},
};

use hyper::Server;
use nix::{fcntl::OFlag, unistd::pipe2};

use config::Config;
use queue::Queue;

pub(crate) struct Snare {
    config: Config,
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

#[tokio::main]
pub async fn main() {
    let config = Config::new();
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let server = match Server::try_bind(&addr) {
        Ok(s) => s,
        Err(e) => fatal_err("Couldn't bind to port", e),
    };

    let (event_read_fd, event_write_fd) = pipe2(OFlag::O_NONBLOCK).unwrap();
    let snare = Arc::new(Snare {
        config,
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

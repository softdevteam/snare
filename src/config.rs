use std::{
    env,
    fs::{canonicalize, read_to_string},
    io::{stderr, Write},
    path::Path,
    process,
};

use getopts::Options;
use secstr::SecStr;

pub struct Config {
    /// The maximum number of parallel jobs to run.
    pub maxjobs: usize,
    /// The port to listen on.
    pub port: u16,
    /// A *fully canonicalised* path to the directory containing per-repo programs.
    pub reposdir: String,
    /// The GitHub secret used to validate requests.
    pub secret: SecStr,
    /// An optional email address to send errors to.
    pub email: Option<String>,
}

impl Config {
    /// Create a `Config`.
    pub fn new() -> Self {
        let args: Vec<String> = env::args().collect();
        let prog = &args[0];
        let matches = Options::new()
            .optmulti("e", "email", "Email address to send errors to", "<email>")
            .optmulti(
                "j",
                "maxjobs",
                "Maximum number of jobs to run at once",
                "<maxjobs>",
            )
            .optmulti("p", "port", "Port to listen on", "<port>")
            .optmulti("r", "reposdir", "Directory of per-repo programs", "<port>")
            .optmulti("s", "secretspath", "Path to secrets", "<path>")
            .optflag("h", "help", "")
            .parse(&args[1..])
            .unwrap_or_else(|_| usage(prog));
        if matches.opt_present("h") {
            usage(prog);
        }

        let email = matches.opt_str("e");

        let maxjobs = matches
            .opt_str("j")
            .map(|x| {
                x.parse()
                    .unwrap_or_else(|_| error("Invalid number of jobs."))
            })
            .unwrap_or_else(num_cpus::get);
        if maxjobs == 0 {
            error("Must allow at least 1 job.");
        } else if maxjobs == std::usize::MAX {
            error(&format!(
                "Maximum number of jobs is {}.",
                std::usize::MAX - 1
            ));
        }

        let port = matches
            .opt_str("p")
            .map(|x| x.parse().unwrap_or_else(|_| error("Invalid port.")))
            .unwrap_or_else(|| error("A port must be specified."));

        let reposdir_str = matches
            .opt_str("r")
            .unwrap_or_else(|| error("A directory for per-repo programs must be specified."));
        let reposdir = match canonicalize(&reposdir_str) {
            Ok(p) => match p.to_str() {
                Some(s) => s.to_owned(),
                None => error(&format!("{}: can't convert to string", &reposdir_str)),
            },
            Err(e) => {
                error(&format!("{}: {}", &reposdir_str, e));
            }
        };

        let secret = {
            let p = &matches
                .opt_str("s")
                .unwrap_or_else(|| error("A secrets path must be specified."));
            let path = Path::new(p);
            SecStr::new(
                read_to_string(path)
                    .unwrap_or_else(|_| error("Couldn't read secrets file."))
                    .trim()
                    .to_owned()
                    .into_bytes(),
            )
        };

        Config {
            maxjobs,
            port,
            reposdir,
            secret,
            email,
        }
    }
}

/// Exit immediately with a message indicating the reason.
fn error(msg: &str) -> ! {
    writeln!(&mut stderr(), "{}", msg).ok();
    process::exit(1)
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

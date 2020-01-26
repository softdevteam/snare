use std::{
    env,
    fs::{canonicalize, read_to_string},
    io::{stderr, Write},
    path::{Path, PathBuf},
    process,
};

use getopts::Options;
use lrlex::lrlex_mod;
use lrpar::{lrpar_mod, Lexeme, Lexer};
use secstr::SecStr;
use users::{get_current_uid, get_user_by_uid, os::unix::UserExt};

use crate::{fatal, fatal_err};

lrlex_mod!("config.l");
lrpar_mod!("config.y");

type StorageT = u8;

/// Default locations to look for `snare.conf`: `~/` will be automatically converted to the current
/// user's home directory.
const SNARE_CONF_SEARCH: &[&str] = &["/etc/snare.conf", "~/.snare.conf"];

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
        let input = match read_to_string(conf_path) {
            Ok(s) => s,
            Err(e) => fatal_err("Can't read configuration file", e),
        };

        let lexerdef = config_l::lexerdef();
        let lexer = lexerdef.lexer(&input);
        let (astopt, errs) = config_y::parse(&lexer);
        for e in &errs {
            eprintln!("{}", e.pp(&lexer, &config_y::token_epp));
        }
        let mut email = None;
        let mut port = None;
        let mut maxjobs = None;
        let mut reposdir = None;
        let mut secret = None;
        match astopt {
            Some(Ok(opts)) => {
                for opt in opts {
                    match opt {
                        GenericOption::Email(lexeme) => {
                            if email.is_some() {
                                conf_fatal(
                                    &lexer,
                                    lexeme,
                                    "Mustn't specify 'email' more than once",
                                );
                            }
                            let email_str = lexer.lexeme_str(&lexeme);
                            let email_str = &email_str[1..email_str.len() - 1];
                            email = Some(email_str.to_owned());
                        }
                        GenericOption::MaxJobs(lexeme) => {
                            if maxjobs.is_some() {
                                conf_fatal(
                                    &lexer,
                                    lexeme,
                                    "Mustn't specify 'maxjobs' more than once",
                                );
                            }
                            let maxjobs_str = lexer.lexeme_str(&lexeme);
                            match maxjobs_str.parse() {
                                Ok(0) => conf_fatal(&lexer, lexeme, "Must allow at least 1 job"),
                                Ok(x) if x == std::usize::MAX => conf_fatal(
                                    &lexer,
                                    lexeme,
                                    &format!("Maximum number of jobs is {}", std::usize::MAX - 1),
                                ),
                                Ok(x) => maxjobs = Some(x),
                                Err(e) => conf_fatal(&lexer, lexeme, &format!("{}", e)),
                            }
                        }
                        GenericOption::Port(lexeme) => {
                            if port.is_some() {
                                conf_fatal(&lexer, lexeme, "Mustn't specify 'port' more than once");
                            }
                            let port_str = lexer.lexeme_str(&lexeme);
                            port = Some(port_str.parse().unwrap_or_else(|_| {
                                conf_fatal(&lexer, lexeme, &format!("Invalid port '{}'", port_str))
                            }));
                        }
                        GenericOption::ReposDir(lexeme) => {
                            if reposdir.is_some() {
                                conf_fatal(
                                    &lexer,
                                    lexeme,
                                    "Mustn't specify 'reposdir' more than once",
                                );
                            }

                            let reposdir_str = lexer.lexeme_str(&lexeme);
                            let reposdir_str = &reposdir_str[1..reposdir_str.len() - 1];
                            reposdir = Some(match canonicalize(reposdir_str) {
                                Ok(p) => match p.to_str() {
                                    Some(s) => s.to_owned(),
                                    None => fatal(&format!(
                                        "'{}': can't convert to string",
                                        &reposdir_str
                                    )),
                                },
                                Err(e) => {
                                    fatal_err(&format!("'{}'", reposdir_str), e);
                                }
                            });
                        }
                        GenericOption::Secret(lexeme) => {
                            if secret.is_some() {
                                conf_fatal(
                                    &lexer,
                                    lexeme,
                                    "Mustn't specify 'secret' more than once",
                                );
                            }
                            let secret_str = lexer.lexeme_str(&lexeme);
                            let secret_str = &secret_str[1..secret_str.len() - 1];
                            secret = Some(SecStr::from(secret_str));
                        }
                    }
                }
            }
            _ => process::exit(1),
        }
        if maxjobs.is_none() {
            maxjobs = Some(num_cpus::get());
        }
        if port.is_none() {
            fatal("A port must be specified");
        }
        if reposdir.is_none() {
            fatal("A directory for per-repo programs must be specified");
        }
        if secret.is_none() {
            fatal("A secret must be specified");
        }

        Config {
            maxjobs: maxjobs.unwrap(),
            port: port.unwrap(),
            reposdir: reposdir.unwrap(),
            secret: secret.unwrap(),
            email,
        }
    }
}

/// Exit with a fatal error message pinpointing `lexeme` as the culprit.
fn conf_fatal(lexer: &dyn Lexer<StorageT>, lexeme: Lexeme<StorageT>, msg: &str) -> ! {
    let (line_off, col) = lexer.line_col(lexeme.start());
    let line = lexer.surrounding_line_str(lexeme.start());
    fatal(&format!(
        "Line {}, column {}:\n  {}\n{}",
        line_off,
        col,
        line.trim(),
        msg
    ));
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

pub enum GenericOption<StorageT> {
    Email(Lexeme<StorageT>),
    MaxJobs(Lexeme<StorageT>),
    Port(Lexeme<StorageT>),
    ReposDir(Lexeme<StorageT>),
    Secret(Lexeme<StorageT>),
}

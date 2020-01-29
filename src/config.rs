use std::{
    env,
    fs::{canonicalize, read_to_string},
    io::{stderr, Write},
    path::{Path, PathBuf},
    process,
};

use crypto_mac::{InvalidKeyLength, Mac};
use getopts::Options;
use hmac::Hmac;
use lrlex::lrlex_mod;
use lrpar::{lrpar_mod, Lexeme, Lexer};
use regex::Regex;
use secstr::SecStr;
use sha1::Sha1;
use users::{get_current_uid, get_user_by_uid, os::unix::UserExt};

use crate::{config_ast, fatal, fatal_err};

type StorageT = u8;

lrlex_mod!("config.l");
lrpar_mod!("config.y");

/// Default locations to look for `snare.conf`: `~/` will be automatically converted to the current
/// user's home directory.
const SNARE_CONF_SEARCH: &[&str] = &["/etc/snare.conf", "~/.snare.conf"];

pub struct Config {
    /// The maximum number of parallel jobs to run.
    pub maxjobs: usize,
    /// The port to listen on.
    pub port: u16,
    /// The GitHub block.
    pub github: GitHub,
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
        let mut github = None;
        let mut port = None;
        let mut maxjobs = None;
        match astopt {
            Some(Ok(opts)) => {
                for opt in opts {
                    match opt {
                        config_ast::TopLevelOption::GitHub(lexeme, options, matches) => {
                            if github.is_some() {
                                conf_fatal(
                                    &lexer,
                                    lexeme,
                                    "Mustn't specify 'github' more than once",
                                );
                            }
                            github = Some(GitHub::parse(&lexer, options, matches));
                        }
                        config_ast::TopLevelOption::MaxJobs(lexeme) => {
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
                        config_ast::TopLevelOption::Port(lexeme) => {
                            if port.is_some() {
                                conf_fatal(&lexer, lexeme, "Mustn't specify 'port' more than once");
                            }
                            let port_str = lexer.lexeme_str(&lexeme);
                            port = Some(port_str.parse().unwrap_or_else(|_| {
                                conf_fatal(&lexer, lexeme, &format!("Invalid port '{}'", port_str))
                            }));
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
        if github.is_none() {
            fatal("A GitHub block with at least a 'repodirs' option must be specified.");
        }

        Config {
            maxjobs: maxjobs.unwrap(),
            port: port.unwrap(),
            github: github.unwrap(),
        }
    }
}

pub struct GitHub {
    /// A *fully canonicalised* path to the directory containing per-repo programs.
    pub reposdir: String,
    pub matches: Vec<Match>,
}

impl GitHub {
    fn parse(
        lexer: &dyn Lexer<StorageT>,
        options: Vec<config_ast::ProviderOption<StorageT>>,
        ast_matches: Vec<config_ast::Match<StorageT>>,
    ) -> Self {
        let mut reposdir = None;
        let mut matches = vec![Match::default()];

        for option in options {
            match option {
                config_ast::ProviderOption::ReposDir(lexeme) => {
                    if reposdir.is_some() {
                        conf_fatal(lexer, lexeme, "Mustn't specify 'reposdir' more than once");
                    }

                    let reposdir_str = lexer.lexeme_str(&lexeme);
                    let reposdir_str = &reposdir_str[1..reposdir_str.len() - 1];
                    reposdir = Some(match canonicalize(reposdir_str) {
                        Ok(p) => match p.to_str() {
                            Some(s) => s.to_owned(),
                            None => fatal(&format!("'{}': can't convert to string", &reposdir_str)),
                        },
                        Err(e) => {
                            fatal_err(&format!("'{}'", reposdir_str), e);
                        }
                    });
                }
            }
        }

        for m in ast_matches {
            let re_str = lexer.lexeme_str(&m.re);
            let re_str = format!("^{}$", &re_str[1..re_str.len() - 1]);
            let re = Regex::new(&re_str).unwrap_or_else(|e| {
                conf_fatal(lexer, m.re, &format!("Regular expression error: {}", e))
            });
            let mut email = None;
            let mut secret = None;
            for opt in m.options {
                match opt {
                    config_ast::PerRepoOption::Email(lexeme) => {
                        if email.is_some() {
                            conf_fatal(lexer, lexeme, "Mustn't specify 'email' more than once");
                        }
                        let email_str = lexer.lexeme_str(&lexeme);
                        let email_str = &email_str[1..email_str.len() - 1];
                        email = Some(email_str.to_owned());
                    }
                    config_ast::PerRepoOption::Secret(lexeme) => {
                        if secret.is_some() {
                            conf_fatal(lexer, lexeme, "Mustn't specify 'secret' more than once");
                        }
                        let sec_str = lexer.lexeme_str(&lexeme);
                        let sec_str = &sec_str[1..sec_str.len() - 1];

                        // Looking at the Hmac code, it seems that a key can't actually be of an
                        // invalid length despite the API suggesting that it can be... We're
                        // conservative and assume that it really is possible to have an invalid
                        // length key.
                        match Hmac::<Sha1>::new_varkey(sec_str.as_bytes()) {
                            Ok(_) => (),
                            Err(InvalidKeyLength) => {
                                conf_fatal(lexer, lexeme, "Invalid secret key length")
                            }
                        }
                        secret = Some(SecStr::from(sec_str));
                    }
                }
            }
            matches.push(Match { re, email, secret });
        }

        let reposdir = reposdir
            .unwrap_or_else(|| fatal("A directory for per-repo programs must be specified"));

        GitHub { reposdir, matches }
    }

    pub fn repoconfig<'a>(&'a self, owner: &str, repo: &str) -> RepoConfig<'a> {
        let s = format!("{}/{}", owner, repo);
        let mut email = None;
        let mut secret = None;
        for m in &self.matches {
            if m.re.is_match(&s) {
                if let Some(ref e) = m.email {
                    email = Some(e.as_str());
                }
                if let Some(ref s) = m.secret {
                    secret = Some(s);
                }
            }
        }
        RepoConfig { email, secret }
    }
}

pub struct Match {
    /// The regular expression to match against full owner/repo names.
    re: Regex,
    /// An optional email address to send errors to.
    email: Option<String>,
    /// The GitHub secret used to validate requests.
    secret: Option<SecStr>,
}

impl Default for Match {
    fn default() -> Self {
        let re = Regex::new(".*").unwrap();
        Match {
            re,
            email: None,
            secret: None,
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

/// The configuration for a given repository.
pub struct RepoConfig<'a> {
    pub email: Option<&'a str>,
    pub secret: Option<&'a SecStr>,
}

use std::{fs::read_to_string, net::SocketAddr, path::PathBuf, process, str::FromStr};

use crypto_mac::{InvalidKeyLength, Mac};
use hmac::Hmac;
use lrlex::lrlex_mod;
use lrpar::{lrpar_mod, Lexer, Span};
use regex::Regex;
use secstr::SecStr;
use sha1::Sha1;

use crate::config_ast;

type StorageT = u8;

const DEFAULT_TIMEOUT: u64 = 60 * 60; // 1 hour

lrlex_mod!("config.l");
lrpar_mod!("config.y");

pub struct Config {
    /// The IP address/port on which to listen.
    pub listen: SocketAddr,
    /// The maximum number of parallel jobs to run.
    pub maxjobs: usize,
    /// The GitHub block.
    pub github: GitHub,
    /// The Unix user to change to after snare has bound itself to a network port.
    pub user: Option<String>,
}

impl Config {
    /// Create a `Config` from `path`, returning `Err(String)` (containing a human readable
    /// message) if it was unable to do so.
    pub fn from_path(conf_path: &PathBuf) -> Result<Self, String> {
        let input = match read_to_string(conf_path) {
            Ok(s) => s,
            Err(e) => return Err(format!("Can't read {:?}: {}", conf_path, e)),
        };

        let lexerdef = config_l::lexerdef();
        let lexer = lexerdef.lexer(&input);
        let (astopt, errs) = config_y::parse(&lexer);
        if !errs.is_empty() {
            let msgs = errs
                .iter()
                .map(|e| e.pp(&lexer, &config_y::token_epp))
                .collect::<Vec<_>>();
            return Err(msgs.join("\n"));
        }
        let mut github = None;
        let mut listen = None;
        let mut maxjobs = None;
        let mut user = None;
        match astopt {
            Some(Ok(opts)) => {
                for opt in opts {
                    match opt {
                        config_ast::TopLevelOption::GitHub(span, options, matches) => {
                            if github.is_some() {
                                return Err(error_at_span(
                                    &lexer,
                                    span,
                                    "Mustn't specify 'github' more than once",
                                ));
                            }
                            github = Some(GitHub::parse(&lexer, options, matches)?);
                        }
                        config_ast::TopLevelOption::Listen(span) => {
                            if listen.is_some() {
                                return Err(error_at_span(
                                    &lexer,
                                    span,
                                    "Mustn't specify 'listen' more than once",
                                ));
                            }
                            let listen_str = unescape_str(lexer.span_str(span));
                            match SocketAddr::from_str(&listen_str) {
                                Ok(l) => listen = Some(l),
                                Err(e) => {
                                    return Err(error_at_span(
                                        &lexer,
                                        span,
                                        &format!("Invalid listen address '{}': {}", listen_str, e),
                                    ));
                                }
                            }
                        }
                        config_ast::TopLevelOption::MaxJobs(span) => {
                            if maxjobs.is_some() {
                                return Err(error_at_span(
                                    &lexer,
                                    span,
                                    "Mustn't specify 'maxjobs' more than once",
                                ));
                            }
                            let maxjobs_str = lexer.span_str(span);
                            match maxjobs_str.parse() {
                                Ok(0) => {
                                    return Err(error_at_span(
                                        &lexer,
                                        span,
                                        "Must allow at least 1 job",
                                    ))
                                }
                                Ok(x) if x > (std::usize::MAX - 1) / 2 => {
                                    return Err(error_at_span(
                                        &lexer,
                                        span,
                                        &format!(
                                            "Maximum number of jobs is {}",
                                            (std::usize::MAX - 1) / 2
                                        ),
                                    ))
                                }
                                Ok(x) => maxjobs = Some(x),
                                Err(e) => {
                                    return Err(error_at_span(&lexer, span, &format!("{}", e)))
                                }
                            }
                        }
                        config_ast::TopLevelOption::User(span) => {
                            if user.is_some() {
                                return Err(error_at_span(
                                    &lexer,
                                    span,
                                    "Mustn't specify 'user' more than once",
                                ));
                            }
                            let user_str = unescape_str(lexer.span_str(span));
                            user = Some(user_str);
                        }
                    }
                }
            }
            _ => process::exit(1),
        }
        let maxjobs = maxjobs.unwrap_or_else(num_cpus::get);
        let listen = listen.ok_or_else(|| "A 'listen' address must be specified".to_owned())?;
        let github = github.ok_or_else(|| {
            "A GitHub block with at least a 'cmd' option must be specified".to_owned()
        })?;

        Ok(Config {
            listen,
            maxjobs,
            github,
            user,
        })
    }
}

pub struct GitHub {
    pub matches: Vec<Match>,
}

impl GitHub {
    fn parse(
        lexer: &dyn Lexer<StorageT>,
        options: Vec<config_ast::ProviderOption>,
        ast_matches: Vec<config_ast::Match>,
    ) -> Result<Self, String> {
        let mut matches = vec![Match::default()];

        if let Some(config_ast::ProviderOption::ReposDir(span)) = options.get(0) {
            return Err(error_at_span(lexer, *span, "Replace:\n  GitHub { reposdir = \"/path/to/reposdir\"; }\nwith:\n  GitHub {\n    match \".*\" {\n      cmd = \"/path/to/reposdir/%o/%r %e %j\";\n    }\n  }"));
        }

        for m in ast_matches {
            let re_str = format!("^{}$", unescape_str(lexer.span_str(m.re)));
            let re = match Regex::new(&re_str) {
                Ok(re) => re,
                Err(e) => {
                    return Err(error_at_span(
                        lexer,
                        m.re,
                        &format!("Regular expression error: {}", e),
                    ))
                }
            };
            let mut cmd = None;
            let mut errorcmd = None;
            let mut queuekind = None;
            let mut secret = None;
            let mut timeout = None;
            for opt in m.options {
                match opt {
                    config_ast::PerRepoOption::Cmd(span) => {
                        if cmd.is_some() {
                            return Err(error_at_span(
                                lexer,
                                span,
                                "Mustn't specify 'cmd' more than once",
                            ));
                        }
                        let cmd_str = unescape_str(lexer.span_str(span));
                        GitHub::verify_cmd_str(&cmd_str)?;
                        cmd = Some(cmd_str);
                    }
                    config_ast::PerRepoOption::Email(span) => {
                        return Err(error_at_span(lexer, span, "Replace:\n  email = \"someone@example.com\"; }\nwith:\n  error_cmd = \"cat %f | mailx -s \\\"snare error: github.com/%o/%r\\\" someone@example.com\";"));
                    }
                    config_ast::PerRepoOption::ErrorCmd(span) => {
                        if errorcmd.is_some() {
                            return Err(error_at_span(
                                lexer,
                                span,
                                "Mustn't specify 'error_cmd' more than once",
                            ));
                        }
                        let errorcmd_str = unescape_str(lexer.span_str(span));
                        GitHub::verify_errorcmd_str(&errorcmd_str)?;
                        errorcmd = Some(errorcmd_str);
                    }
                    config_ast::PerRepoOption::Queue(span, qkind) => {
                        if queuekind.is_some() {
                            return Err(error_at_span(
                                lexer,
                                span,
                                "Mustn't specify 'queue' more than once",
                            ));
                        }
                        queuekind = Some(match qkind {
                            config_ast::QueueKind::Evict => QueueKind::Evict,
                            config_ast::QueueKind::Parallel => QueueKind::Parallel,
                            config_ast::QueueKind::Sequential => QueueKind::Sequential,
                        });
                    }
                    config_ast::PerRepoOption::Secret(span) => {
                        if secret.is_some() {
                            return Err(error_at_span(
                                lexer,
                                span,
                                "Mustn't specify 'secret' more than once",
                            ));
                        }
                        let sec_str = unescape_str(lexer.span_str(span));

                        // Looking at the Hmac code, it seems that a key can't actually be of an
                        // invalid length despite the API suggesting that it can be... We're
                        // conservative and assume that it really is possible to have an invalid
                        // length key.
                        match Hmac::<Sha1>::new_varkey(sec_str.as_bytes()) {
                            Ok(_) => (),
                            Err(InvalidKeyLength) => {
                                return Err(error_at_span(lexer, span, "Invalid secret key length"))
                            }
                        }
                        secret = Some(SecStr::from(sec_str));
                    }
                    config_ast::PerRepoOption::Timeout(span) => {
                        if timeout.is_some() {
                            return Err(error_at_span(
                                lexer,
                                span,
                                "Mustn't specify 'timeout' more than once",
                            ));
                        }
                        let t = match lexer.span_str(span).parse() {
                            Ok(t) => t,
                            Err(e) => {
                                return Err(error_at_span(
                                    lexer,
                                    span,
                                    &format!("Invalid timeout: {}", e),
                                ))
                            }
                        };
                        timeout = Some(t);
                    }
                }
            }
            matches.push(Match {
                re,
                cmd,
                errorcmd,
                queuekind,
                secret,
                timeout,
            });
        }

        Ok(GitHub { matches })
    }

    /// Verify that the `cmd` string is valid, returning `Ok())` if so or `Err(String)` if not.
    fn verify_cmd_str(cmd: &str) -> Result<(), String> {
        GitHub::verify_str(cmd, &['e', 'o', 'r', 'j', '%'])
    }

    /// Verify that the `errorcmd` string is valid, returning `Ok())` if so or `Err(String)` if not.
    fn verify_errorcmd_str(errorcmd: &str) -> Result<(), String> {
        GitHub::verify_str(errorcmd, &['e', 'o', 'r', 'j', 's', '%'])
    }

    fn verify_str(s: &str, modifiers: &[char]) -> Result<(), String> {
        let mut i = 0;
        while i < s.len() {
            if s[i..].starts_with('%') {
                if i + 1 == s.len() {
                    return Err("Cannot end command string with a single '%'.".to_owned());
                }
                let c = s[i + 1..].chars().next().unwrap();
                if !modifiers.contains(&c) {
                    return Err(format!("Unknown '%' modifier '{}.", c));
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        Ok(())
    }

    /// Return a `RepoConfig` for `owner/repo`. Note that if the user reloads the config later,
    /// then a given repository might have two or more `RepoConfig`s with internal settings, so
    /// they should not be mixed. We return the repository's secret as a separate member as it is
    /// relatively costly to clone, and we also prefer not to duplicate it repeatedly throughout
    /// the heap.
    pub fn repoconfig<'a>(&'a self, owner: &str, repo: &str) -> (RepoConfig, Option<&'a SecStr>) {
        let s = format!("{}/{}", owner, repo);
        let mut cmd = None;
        let mut errorcmd = None;
        let mut queuekind = None;
        let mut secret = None;
        let mut timeout = None;
        for m in &self.matches {
            if m.re.is_match(&s) {
                if let Some(ref c) = m.cmd {
                    cmd = Some(c.clone());
                }
                if let Some(ref e) = m.errorcmd {
                    errorcmd = Some(e.clone());
                }
                if let Some(q) = m.queuekind {
                    queuekind = Some(q);
                }
                if let Some(ref s) = m.secret {
                    secret = Some(s);
                }
                if let Some(t) = m.timeout {
                    timeout = Some(t)
                }
            }
        }
        // Since we know that Matches::default() provides a default queuekind and timeout, both
        // unwraps() are safe.
        (
            RepoConfig {
                cmd,
                errorcmd,
                queuekind: queuekind.unwrap(),
                timeout: timeout.unwrap(),
            },
            secret,
        )
    }
}

/// Take a quoted string from the config file and unescape it (i.e. strip the start and end quote
/// (") characters and process any escape characters in the string.)
fn unescape_str(us: &str) -> String {
    // The regex in config.l should have guaranteed that strings start and finish with a
    // quote character.
    debug_assert!(us.starts_with('"') && us.ends_with('"'));
    let mut s = String::new();
    // We iterate over all characters except the opening and closing quote characters.
    let mut i = '"'.len_utf8();
    while i < us.len() - '"'.len_utf8() {
        let c = us[i..].chars().next().unwrap();
        if c == '\\' {
            // The regex in config.l should have guaranteed that there are no unescaped quote (")
            // characters, but we check here just to be sure.
            debug_assert!(i < us.len() - '"'.len_utf8());
            i += 1;
            let c2 = us[i..].chars().next().unwrap();
            debug_assert!(c2 == '"' || c2 == '\\');
            s.push(c2);
            i += c2.len_utf8();
        } else {
            s.push(c);
            i += c.len_utf8();
        }
    }
    s
}

pub struct Match {
    /// The regular expression to match against full owner/repo names.
    re: Regex,
    /// The command to run (note that this contains escape characters such as %o and %r).
    cmd: Option<String>,
    /// An optional command to run when an error occurs (note that this contains escape characters
    /// such as %o and %r).
    errorcmd: Option<String>,
    /// The queue kind.
    queuekind: Option<QueueKind>,
    /// The GitHub secret used to validate requests.
    secret: Option<SecStr>,
    /// The maximum time to allow a command to run for before it is terminated (in seconds).
    timeout: Option<u64>,
}

impl Default for Match {
    fn default() -> Self {
        // We know that this Regex is valid so the unwrap() is safe.
        let re = Regex::new(".*").unwrap();
        Match {
            re,
            cmd: None,
            errorcmd: None,
            queuekind: Some(QueueKind::Sequential),
            secret: None,
            timeout: Some(DEFAULT_TIMEOUT),
        }
    }
}

/// Return an error message pinpointing `span` as the culprit.
fn error_at_span(lexer: &dyn Lexer<StorageT>, span: Span, msg: &str) -> String {
    let ((line_off, col), _) = lexer.line_col(span);
    let code = lexer
        .span_lines_str(span)
        .split('\n')
        .next()
        .unwrap()
        .trim();
    format!(
        "Line {}, column {}:\n  {}\n{}",
        line_off,
        col,
        code.trim(),
        msg
    )
}

/// The configuration for a given repository.
pub struct RepoConfig {
    pub cmd: Option<String>,
    pub errorcmd: Option<String>,
    pub queuekind: QueueKind,
    pub timeout: u64,
}

#[derive(Clone, Copy)]
pub enum QueueKind {
    Evict,
    Parallel,
    Sequential,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_verify_cmd_string() {
        assert!(GitHub::verify_cmd_str("").is_ok());
        assert!(GitHub::verify_cmd_str("a").is_ok());
        assert!(GitHub::verify_cmd_str("%% %e %o %r %j %%").is_ok());
        assert!(GitHub::verify_cmd_str("%%").is_ok());
        assert!(GitHub::verify_cmd_str("%").is_err());
        assert!(GitHub::verify_cmd_str("a%").is_err());
        assert!(GitHub::verify_cmd_str("%a").is_err());
        assert!(GitHub::verify_cmd_str("%s").is_err());
    }

    #[test]
    fn test_verify_errorcmd_string() {
        assert!(GitHub::verify_errorcmd_str("").is_ok());
        assert!(GitHub::verify_errorcmd_str("a").is_ok());
        assert!(GitHub::verify_errorcmd_str("%% %e %o %r %j %s %%").is_ok());
        assert!(GitHub::verify_errorcmd_str("%%").is_ok());
        assert!(GitHub::verify_errorcmd_str("%").is_err());
        assert!(GitHub::verify_errorcmd_str("a%").is_err());
        assert!(GitHub::verify_errorcmd_str("%a").is_err());
    }

    #[test]
    fn test_unescape_string() {
        assert_eq!(unescape_str("\"\""), "");
        assert_eq!(unescape_str("\"a\""), "a");
        assert_eq!(unescape_str("\"a\\\"\""), "a\"");
        assert_eq!(unescape_str("\"a\\\"b\""), "a\"b");
        assert_eq!(unescape_str("\"\\\\\""), "\\");
    }

    #[test]
    fn test_example_conf() {
        let mut p = PathBuf::new();
        p.push(env!("CARGO_MANIFEST_DIR"));
        p.push("snare.conf.example");
        match Config::from_path(&p) {
            Ok(_) => (),
            Err(e) => panic!("{:?}", e),
        }
    }
}

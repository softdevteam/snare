use std::{
    collections::HashMap,
    error::Error,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use hmac::{Hmac, Mac};
use log::{error, trace};
use percent_encoding::percent_decode;
use secstr::SecStr;
use sha2::Sha256;

use crate::{queue::QueueJob, Snare};

/// How many connections to accept simultaneously? Limiting this number stops attackers from
/// causing us to use too many resources.
static MAX_SIMULTANEOUS_CONNECTIONS: usize = 16;
/// How long to try reading/writing from a socket before we assume it's died.
static NET_TIMEOUT: Duration = Duration::from_secs(10);
/// The maximum payload size we'll accept from a remote in bytes. The main reason to limit this is
/// to stop large numbers of requests causing us to run out of memory.
static MAX_HTTP_BODY_SIZE: usize = 64 * 1024;

pub(crate) fn serve(snare: Arc<Snare>) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(snare.conf.lock().unwrap().listen)?;
    #[cfg(feature = "_internal_testing")]
    {
        if let Ok(p) = std::env::var("SNARE_DEBUG_PORT_PATH") {
            std::fs::write(p, &listener.local_addr().unwrap().port().to_string()).unwrap();
        }
    }

    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        // We want to keep a limit on how many threads are started concurrently, so that an
        // attacker can't DOS the machine. `active` keeps track of how many threads are (or are
        // just about to be) active. Since the common case is that we haven't hit the limit, we
        // speculatively `fetch_add` and, if that fails, we then "undo" that with a `fetch_sub`,
        // wait and try again. [Since the main thread is the only thread incrementing the count
        // we could do things like a `load`, a check, and then a `fetch_add`, but that requires
        // two atomic operations, so is slower, and also more fragile if we refactor the code in
        // the future.]
        while active.fetch_add(1, Ordering::Relaxed) > MAX_SIMULTANEOUS_CONNECTIONS {
            active.fetch_sub(1, Ordering::Relaxed);
            // We only expect to hit this loop if someone is doing something very odd, so the time
            // we wait isn't particularly important.
            thread::sleep(Duration::from_millis(100));
        }

        let active = Arc::clone(&active);
        let snare = Arc::clone(&snare);
        thread::spawn(move || {
            request(&snare, stream);
            active.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

/// Try processing an HTTP request.
fn request(snare: &Arc<Snare>, mut stream: TcpStream) {
    match (
        stream.set_read_timeout(Some(NET_TIMEOUT)),
        stream.set_write_timeout(Some(NET_TIMEOUT)),
    ) {
        (Ok(_), Ok(_)) => (),
        _ => {
            error!("Couldn't set timeout on sockets");
            http_500(stream);
            return;
        }
    }
    let req_time = Instant::now();
    let (headers, body) = match parse_get(&mut stream) {
        Ok(x) => x,
        Err(e) => {
            trace!("Processing HTTP request: {e}");
            http_400(stream);
            return;
        }
    };
    if stream.shutdown(Shutdown::Read).is_err() {
        http_400(stream);
        return;
    }

    let event_type = match headers.get("x-github-event") {
        Some(x) => x,
        None => {
            trace!("HTTP request: X-Github-Event header missing");
            http_400(stream);
            return;
        }
    };
    if !valid_github_event(event_type) {
        error!("Invalid GitHub event type '{event_type}'");
        http_400(stream);
        return;
    }
    let sig = match headers
        .get("x-hub-signature-256")
        .and_then(|s| s.split_once('='))
    {
        Some(("sha256", sig)) => Some(sig),
        Some(_) => {
            trace!("Incorrectly formatted X-Hub-Signature-256 header");
            http_400(stream);
            return;
        }
        None => None,
    };

    if !body.starts_with("payload=".as_bytes()) {
        trace!("Payload does not start with 'payload='");
        http_400(stream);
        return;
    }
    let json_str = match percent_decode(&body[8..]).decode_utf8() {
        Ok(x) => x.to_string(),
        Err(_) => {
            trace!("JSON not valid UTF-8");
            http_400(stream);
            return;
        }
    };
    let jv = match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(x) => x,
        Err(e) => {
            trace!("Can't parse JSON: {e}");
            http_400(stream);
            return;
        }
    };
    let (owner, repo) = match (
        &jv["repository"]["owner"]["login"].as_str(),
        &jv["repository"]["name"].as_str(),
    ) {
        (Some(o), Some(r)) => (o.to_owned(), r.to_owned()),
        _ => {
            trace!("Invalid JSON");
            http_400(stream);
            return;
        }
    };

    if !valid_github_ownername(owner) {
        trace!("Invalid GitHub owner syntax '{owner}'.");
        http_400(stream);
        return;
    }
    if !valid_github_reponame(repo) {
        trace!("Invalid GitHub repository syntax '{repo}'.");
        http_400(stream);
        return;
    }

    let conf = snare.conf.lock().unwrap();
    let (rconf, secret) = conf.github.repoconfig(owner, repo);

    match (secret, sig) {
        (Some(secret), Some(sig)) => {
            if !authenticate(secret, sig, &body) {
                error!("Authentication failed for {owner}/{repo}.");
                http_401(stream);
                return;
            }
        }
        (Some(_), None) => {
            error!("Secret specified but request unsigned");
            http_401(stream);
            return;
        }
        (None, Some(_)) => {
            error!("Request was signed but no secret was specified for {owner}/{repo}.");
            http_401(stream);
            return;
        }
        (None, None) => (),
    }
    drop(conf);

    if event_type == "ping" {
        return;
    }

    let repo_id = format!("github/{}/{}", owner, repo);
    let qj = QueueJob::new(
        repo_id,
        owner.to_owned(),
        repo.to_owned(),
        req_time,
        event_type.to_owned(),
        json_str,
        rconf,
    );
    (*snare.queue.lock().unwrap()).push_back(qj);
    // If the write fails, it almost certainly means that the pipe is full i.e. the runner
    // thread will be notified anyway. If something else happens to have gone wrong, then
    // we (and the OS) are probably in deep trouble anyway...
    nix::unistd::write(snare.event_write_fd, &[0]).ok();

    http_200(stream);
}

/// A very literal, and rather unforgiving, implementation of RFC2616 (HTTP/1.1), returning the URL
/// of GET requests: returns `Err` for anything else.
fn parse_get(stream: &mut TcpStream) -> Result<(HashMap<String, String>, Vec<u8>), Box<dyn Error>> {
    let mut rdr = BufReader::new(stream);
    let mut req_line = String::new();
    rdr.read_line(&mut req_line)?;

    // First the request line:
    //   Request-Line   = Method SP Request-URI SP HTTP-Version CRLF
    // where Method = "POST" and `SP` is a single space character.
    let req_line_sp = req_line.split(' ').collect::<Vec<_>>();
    if !matches!(req_line_sp.as_slice(), &["POST", _, _]) {
        return Err("Not a POST query".into());
    }

    // Consume rest of HTTP request
    let mut headers: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        rdr.read_line(&mut line)?;
        if line.as_str().trim().is_empty() {
            break;
        }
        match line.chars().next() {
            Some(' ') | Some('\t') => {
                // Continuation of previous header
                match headers.last_mut() {
                    Some(x) => {
                        // Not calling `trim_start` means that the two joined lines have at least
                        // one space|tab between them.
                        x.push_str(line.as_str().trim_end());
                    }
                    None => return Err("Malformed HTTP header".into()),
                }
            }
            _ => headers.push(line.as_str().trim_end().to_owned()),
        }
    }
    let mut headers_map = HashMap::with_capacity(headers.len());
    for x in headers {
        match x.splitn(2, ':').collect::<Vec<_>>().as_slice() {
            &[k, v] => {
                headers_map.insert(k.to_lowercase(), v.trim_start().to_owned());
            }
            _ => return Err("Malformed HTTP headers".into()),
        }
    }

    let len = headers_map
        .get("content-length")
        .ok_or_else(|| "Missing 'Content-Length' header".to_owned())?
        .parse::<usize>()?;
    if len > MAX_HTTP_BODY_SIZE {
        return Err(format!("Body of {len} bytes too big").into());
    }
    let mut body = vec![0; len];
    rdr.read_exact(&mut body)?;

    Ok((headers_map, body))
}

fn http_200(mut stream: TcpStream) {
    stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").ok();
}

fn http_400(mut stream: TcpStream) {
    stream.write_all(b"HTTP/1.1 400\r\n\r\n").ok();
}

fn http_401(mut stream: TcpStream) {
    stream.write_all(b"HTTP/1.1 401\r\n\r\n").ok();
}

fn http_500(mut stream: TcpStream) {
    stream.write_all(b"HTTP/1.1 500\r\n\r\n").ok();
}

/// Authenticate this request and if successful return `true` (where "success" also includes "the
/// user didn't specify a secret for this repository").
fn authenticate(secret: &SecStr, sig: &str, pl: &[u8]) -> bool {
    // We've already checked the key length when creating the config, so the unwrap() is safe.
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.unsecure()).unwrap();
    mac.update(pl);
    match hex::decode(sig) {
        Ok(d) => mac.verify_slice(&d).is_ok(),
        Err(_) => false,
    }
}

/// Is `t` a valid GitHub event type? If this function returns `true` then it is guaranteed that `t`
/// is safe to use in file system paths.
fn valid_github_event(t: &str) -> bool {
    // All current event types are [a-z_] https://developer.github.com/webhooks/
    !t.is_empty() && t.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

/// Is `n` a valid GitHub ownername? If this function returns `true` then it is guaranteed that `n`
/// is safe to use in file system paths.
fn valid_github_ownername(n: &str) -> bool {
    // You can see the rules by going to https://github.com/join, typing in something incorrect and
    // then being told the rules.

    // Owner names must be at least one, and at most 39, characters long.
    if n.is_empty() || n.len() > 39 {
        return false;
    }

    // Owner names cannot start or end with a hyphen.
    if n.starts_with('-') || n.ends_with('-') {
        return false;
    }

    // Owner names cannot contain double hypens.
    if n.contains("--") {
        return false;
    }

    // All characters must be [a-zA-Z0-9-].
    n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Is `n` a valid GitHub repository name? If this function returns `true` then it is guaranteed that `n`
/// is safe to use in filesystem paths.
fn valid_github_reponame(n: &str) -> bool {
    // You can see the rules by going to https://github.com/new, typing in something incorrect and
    // then being told the rules.

    // A repository name must be at least 1, at most 100, characters long.
    if n.is_empty() || n.len() > 100 {
        return false;
    }

    // GitHub disallows repository names "." and ".."
    if n == "." || n == ".." {
        return false;
    }

    // All characters must be [a-zA-Z0-9-.]
    n.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn github_event() {
        assert!(!valid_github_event(""));
        assert!(valid_github_event("a"));
        assert!(valid_github_event("check_run"));
        assert!(!valid_github_event("check-run"));
        assert!(!valid_github_event("check-run2"));

        let mut s = String::new();
        for i in 0..255 {
            let c = char::from(i);
            if c.is_ascii_lowercase() || c == '_' {
                continue;
            }
            s.clear();
            s.push(c);
            assert!(!valid_github_event(&s));
        }
    }

    #[test]
    fn github_ownername() {
        assert!(!valid_github_ownername(""));
        assert!(valid_github_ownername("a"));
        assert!(!valid_github_ownername("-a"));
        assert!(!valid_github_ownername("-a-"));
        assert!(!valid_github_ownername("a-"));

        assert!(valid_github_ownername(
            "123456789012345678901234567890123456789"
        ));
        assert!(!valid_github_ownername(
            "1234567890123456789012345678901234567890"
        ));
        assert!(!valid_github_ownername(
            "12345678901234567890123456789012345678-"
        ));
        assert!(!valid_github_ownername(
            "-23456789012345678901234567890123456780"
        ));

        assert!(valid_github_ownername("a-b"));
        assert!(!valid_github_ownername("a--b"));

        assert!(valid_github_ownername("A"));

        let mut s = String::new();
        for i in 0..255 {
            let c = char::from(i);
            if c.is_ascii_alphanumeric() {
                continue;
            }
            s.clear();
            s.push(c);
            assert!(!valid_github_ownername(&s));
        }
    }

    #[test]
    fn github_reponame() {
        assert!(!valid_github_reponame(""));
        assert!(!valid_github_reponame("."));
        assert!(!valid_github_reponame(".."));
        assert!(valid_github_reponame("..."));

        assert!(valid_github_reponame("a"));
        assert!(valid_github_reponame("-"));
        assert!(valid_github_reponame("_"));
        assert!(valid_github_reponame("-.-"));

        let mut s = String::new();
        for i in 0..255 {
            let c = char::from(i);
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                continue;
            }
            s.clear();
            s.push(c);
            assert!(!valid_github_reponame(&s));
        }
    }
}

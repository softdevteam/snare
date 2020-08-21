use std::{convert::Infallible, sync::Arc, time::Instant};

use crypto_mac::Mac;
use hmac::Hmac;
use hyper::service::{make_service_fn, service_fn};
use hyper::{self, body::Bytes, server::conn::AddrIncoming, Body, Request, Response, StatusCode};
use percent_encoding::percent_decode;
use secstr::SecStr;
use sha1::Sha1;

use crate::{queue::QueueJob, Snare};

pub(crate) async fn serve(server: hyper::server::Builder<AddrIncoming>, snare: Arc<Snare>) {
    let make_svc = make_service_fn(|_| {
        let snare = Arc::clone(&snare);
        async { Ok::<_, Infallible>(service_fn(move |req| handle(req, Arc::clone(&snare)))) }
    });

    if let Err(e) = server.serve(make_svc).await {
        snare.fatal_err("Couldn't start HTTP server", e);
    }
}

async fn handle(req: Request<Body>, snare: Arc<Snare>) -> Result<Response<Body>, Infallible> {
    let mut res = Response::new(Body::empty());
    let req_time = Instant::now();
    let event_type = match req.headers().get("X-GitHub-Event") {
        Some(hv) => match hv.to_str() {
            Ok(s) => {
                if !valid_github_event(s) {
                    snare.error(&format!("Invalid GitHub event type '{}'.", s));
                    *res.status_mut() = StatusCode::BAD_REQUEST;
                    return Ok(res);
                }
                s.to_owned()
            }
            Err(_) => {
                *res.status_mut() = StatusCode::BAD_REQUEST;
                return Ok(res);
            }
        },
        None => {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }
    };

    // Extract the string 'def' from "X-Hub-Signature: abc=def" if the header is present.
    let sig = if req.headers().contains_key("X-Hub-Signature") {
        if let Some(sig) = get_hub_sig(&req) {
            Some(sig)
        } else {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }
    } else {
        None
    };

    let (pl, json_str, owner, repo) = match parse(req).await {
        Ok((pl, j, o, r)) => (pl, j, o, r),
        Err(_) => {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }
    };

    if !valid_github_ownername(&owner) {
        snare.error(&format!("Invalid GitHub owner '{}'.", &owner));
        *res.status_mut() = StatusCode::BAD_REQUEST;
        return Ok(res);
    } else if !valid_github_reponame(&repo) {
        snare.error(&format!("Invalid GitHub repository '{}'.", &repo));
        *res.status_mut() = StatusCode::BAD_REQUEST;
        return Ok(res);
    }

    let conf = snare.conf.lock().unwrap();
    let (rconf, secret) = conf.github.repoconfig(&owner, &repo);

    match (secret, sig) {
        (Some(secret), Some(sig)) => {
            if !authenticate(secret, sig, pl) {
                snare.error(&format!("Authentication failed for {}/{}.", owner, repo));
                *res.status_mut() = StatusCode::UNAUTHORIZED;
                return Ok(res);
            }
        }
        (Some(_), None) => {
            snare.error(&format!("Request was unsigned for {}/{}.", owner, repo));
            *res.status_mut() = StatusCode::UNAUTHORIZED;
            return Ok(res);
        }
        (None, Some(_)) => {
            snare.error(&format!(
                "Request was signed but no secret was specified for {}/{}.",
                owner, repo
            ));
            *res.status_mut() = StatusCode::UNAUTHORIZED;
            return Ok(res);
        }
        (None, None) => (),
    }

    if event_type == "ping" {
        *res.status_mut() = StatusCode::OK;
        return Ok(res);
    }

    let repo_id = format!("github/{}/{}", owner, repo);
    let qj = QueueJob::new(repo_id, owner, repo, req_time, event_type, json_str, rconf);
    (*snare.queue.lock().unwrap()).push_back(qj);
    *res.status_mut() = StatusCode::OK;
    // If the write fails, it almost certainly means that the pipe is full i.e. the runner
    // thread will be notified anyway. If something else happens to have gone wrong, then
    // we (and the OS) are probably in deep trouble anyway...
    nix::unistd::write(snare.event_write_fd, &[0]).ok();
    Ok(res)
}

/// Extract the string 'def' from "X-Hub-Signature: abc=def"
fn get_hub_sig(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get("X-Hub-Signature")
        .and_then(|s| match s.to_str() {
            Ok(s) => Some(s),
            Err(_) => None,
        })
        .and_then(|s| s.split('=').nth(1))
        .map(|s| s.to_owned())
}

/// Authenticate this request and if successful return `true` (where "success" also includes "the
/// user didn't specify a secret for this repository").
fn authenticate(secret: &SecStr, sig: String, pl: Bytes) -> bool {
    // We've already checked the key length when creating the config, so the unwrap() is safe.
    let mut mac = Hmac::<Sha1>::new_varkey(secret.unsecure()).unwrap();
    mac.input(&*pl);
    match hex::decode(sig) {
        Ok(d) => mac.verify(&d).is_ok(),
        Err(_) => false,
    }
}

/// Parse `pl` into JSON, and return `(<JSON as a String>, <repo owner>, <repo name>)`.
async fn parse(req: Request<Body>) -> Result<(Bytes, String, String, String), ()> {
    let pl = hyper::body::to_bytes(req.into_body())
        .await
        .map_err(|_| ())?;

    // The body sent by GitHub starts "payload=" before then containing JSON encoded using the URL
    // percent format.

    // First check that the string really does begin "payload=".
    if pl.len() < 8 {
        return Err(());
    }
    match std::str::from_utf8(&pl[..8]) {
        Ok(s) if s == "payload=" => (),
        _ => return Err(()),
    }

    // Decode the JSON and extract the owner and repo.
    let json_str = percent_decode(&pl[8..])
        .decode_utf8()
        .map_err(|_| ())?
        .into_owned();
    let jv = json::parse(&json_str).map_err(|_| ())?;
    let owner_json = &jv["repository"]["owner"]["login"];
    let repo_json = &jv["repository"]["name"];
    match (owner_json.as_str(), repo_json.as_str()) {
        (Some(o), Some(r)) => Ok((pl, json_str, o.to_owned(), r.to_owned())),
        _ => Err(()),
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

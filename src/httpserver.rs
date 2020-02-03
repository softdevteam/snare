use std::{convert::Infallible, path::PathBuf, sync::Arc, time::Instant};

use crypto_mac::Mac;
use hex;
use hmac::Hmac;
use hyper::service::{make_service_fn, service_fn};
use hyper::{self, body::Bytes, server::conn::AddrIncoming, Body, Request, Response, StatusCode};
use json;
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
            Ok(s) => s.to_owned(),
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

    // Extract the string 'def' from "X-Hub-Signature: abc=def"
    let sig = match get_hub_sig(&req) {
        Ok(s) => s,
        Err(()) => {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }
    };

    let (pl, json_str, owner, repo) = match parse(req).await {
        Ok((pl, j, o, r)) => (pl, j, o, r),
        Err(_) => {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }
    };

    // If the unwrap() on the lock fails, the other thread has paniced.
    let conf = snare.conf.lock().unwrap();
    let (rconf, secret) = conf.github.repoconfig(&owner, &repo);

    if !authenticate(secret, sig, pl) {
        *res.status_mut() = StatusCode::UNAUTHORIZED;
        return Ok(res);
    }

    if event_type == "ping" {
        *res.status_mut() = StatusCode::OK;
        return Ok(res);
    }

    // We now check that there is a per-repo program for this repository and that we haven't been
    // tricked into searching for a file outside of the repos dir.
    let mut p = PathBuf::new();
    p.push(&conf.github.reposdir);
    p.push(owner);
    p.push(repo);
    if let Ok(p) = p.canonicalize() {
        if let Some(s) = p.to_str() {
            if s.starts_with(&conf.github.reposdir) {
                // We can tolerate the `unwrap` call below because if it fails it means that
                // something has gone so seriously wrong in the other thread that there's no
                // likelihood that we can recover.
                let qj = QueueJob::new(s.to_owned(), req_time, event_type, json_str, rconf);
                (*snare.queue.lock().unwrap()).push_back(qj);
                *res.status_mut() = StatusCode::OK;
                // If the write fails, it almost certainly means that the pipe is full i.e. the
                // runner thread will be notified anyway. If something else happens to have gone
                // wrong, then we (and the OS) are probably in deep trouble anyway...
                nix::unistd::write(snare.event_write_fd, &[0]).ok();
                return Ok(res);
            }
        }
    }

    // We couldn't find a per-repo program for this request.
    *res.status_mut() = StatusCode::BAD_REQUEST;
    Ok(res)
}

/// Extract the string 'def' from "X-Hub-Signature: abc=def"
fn get_hub_sig(req: &Request<Body>) -> Result<String, ()> {
    Ok(req
        .headers()
        .get("X-Hub-Signature")
        .ok_or(())?
        .to_str()
        .map_err(|_| ())?
        .split('=')
        .nth(1)
        .ok_or(())?
        .to_owned())
}

/// Authenticate this request and if successful return `true` (where "success" also includes "the
/// user didn't specify a secret for this repository").
fn authenticate(secret: Option<&SecStr>, sig: String, pl: Bytes) -> bool {
    if let Some(ref sec) = secret {
        // We've already checked the key length when creating the config, so the unwrap() is safe.
        let mut mac = Hmac::<Sha1>::new_varkey(sec.unsecure()).unwrap();
        mac.input(&*pl);
        match hex::decode(sig) {
            Ok(d) => {
                if mac.verify(&d).is_ok() {
                    return true;
                }
                return false;
            }
            Err(_) => return false,
        }
    }
    true
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

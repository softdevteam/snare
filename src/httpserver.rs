use std::{convert::Infallible, path::PathBuf, process, sync::Arc, time::Instant};

use hex;
use hmac::{Hmac, Mac};
use hyper::service::{make_service_fn, service_fn};
use hyper::{self, body::Bytes, server::conn::AddrIncoming, Body, Request, Response, StatusCode};
use json;
use percent_encoding::percent_decode;
use sha1::Sha1;

use crate::{queue::QueueJob, Snare};

pub(crate) async fn serve(server: hyper::server::Builder<AddrIncoming>, snare: Arc<Snare>) {
    let make_svc = make_service_fn(|_| {
        let snare = Arc::clone(&snare);
        async { Ok::<_, Infallible>(service_fn(move |req| handle(req, Arc::clone(&snare)))) }
    });

    if let Err(e) = server.serve(make_svc).await {
        eprintln!("Warning: {}", e);
        process::exit(1)
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

    let pl = match authenticate(req, &snare).await {
        Ok(p) => p,
        Err(_) => {
            *res.status_mut() = StatusCode::UNAUTHORIZED;
            return Ok(res);
        }
    };

    let (json_str, owner, repo) = match parse(pl) {
        Ok((j, o, r)) => (j, o, r),
        Err(_) => {
            *res.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(res);
        }
    };

    // We now want to find the per-repo program to run while making sure that we aren't tricked into
    // executing a file outside of the repos dir.
    let mut p = PathBuf::new();
    p.push(&snare.config.reposdir);
    p.push(owner);
    p.push(repo);
    if let Ok(p) = p.canonicalize() {
        if let Some(s) = p.to_str() {
            if s.starts_with(&snare.config.reposdir) {
                // We can tolerate the `unwrap` call below because if it fails it means that
                // something has gone so seriously wrong in the other thread that there's no
                // likelihood that we can recover.
                let qj = QueueJob::new(s.to_owned(), req_time, event_type, json_str);
                (*snare.queue.lock().unwrap()).push_back(qj);
                *res.status_mut() = StatusCode::OK;
                // If the write fails, it almost certainly means that the pipe is full i.e. the
                // runner thread will be notified anyway. Just in case something else very odd
                // happened, the runner thread periodically checks the queue "just in case", so
                // even if this write does fail, the job will be picked up in the not too distant
                // future. In either case, a failed write doesn't stop progress being made.
                nix::unistd::write(snare.event_write_fd, &[0]).ok();
                return Ok(res);
            }
        }
    }
    // We couldn't find a per-repo program for this request.
    *res.status_mut() = StatusCode::BAD_REQUEST;
    Ok(res)
}

/// Authenticate this request. If successful, return the body of the request. Any error at all
/// leads to a blanket `Err(())` so that an attacker cannot deduce anything about the cause of the
/// failure to authenticate.
async fn authenticate(req: Request<Body>, snare: &Arc<Snare>) -> Result<Bytes, ()> {
    // Extract the string 'def' from "X-Hub-Signature: abc=def"
    let sig = req
        .headers()
        .get("X-Hub-Signature")
        .ok_or(())?
        .to_str()
        .map_err(|_| ())?
        .split('=')
        .nth(1)
        .ok_or(())?
        .to_owned();

    let data = hyper::body::to_bytes(req.into_body())
        .await
        .map_err(|_| ())?;

    // Verify that this request was signed by the same secret that we have.
    let mut mac = Hmac::<Sha1>::new_varkey(snare.config.secret.unsecure()).map_err(|_| ())?;
    mac.input(&*data);
    mac.verify(&hex::decode(sig).map_err(|_| ())?)
        .map_err(|_| ())?;

    Ok(data)
}

/// Parse `pl` into JSON, and return `(<JSON as a String>, <repo owner>, <repo name>)`.
fn parse(pl: Bytes) -> Result<(String, String, String), ()> {
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
        (Some(o), Some(r)) => Ok((json_str, o.to_owned(), r.to_owned())),
        _ => Err(()),
    }
}

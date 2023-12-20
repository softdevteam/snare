use std::{error::Error, fs::read_dir, thread::sleep};
use tempfile::Builder;

mod common;
use common::run_success;

// Note that `sleep_s` has to be a fairly high value as we really hope that snare has finished
// processing all the jobs we've thrown at it. There's no easy way to do that other than waiting
// for "longer than we expect to see in practise".
fn run_queue(
    queue_kind: &str,
    repeat: usize,
    sh_sleep: &str,
    sleep_s: u64,
) -> Result<usize, Box<dyn Error>> {
    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let tds = td.path().to_str().unwrap();

    let mut reqs = Vec::new();
    for i in 0..repeat {
        reqs.push((
            move |port| {
                Ok(format!(
                    r#"POST /payload HTTP/1.1
Host: 127.0.0.1:{port}
Content-Length: 104
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256=292e1ce3568fecd98589c71938e19afee9b04b7fe11886d5478d802416bbde66
User-Agent: GitHub-Hookshot/044aadd
Content-Type: application/json
X-GitHub-Event: issues
X-GitHub-Hook-ID: 292430182
X-GitHub-Hook-Installation-Target-ID: 79929171
X-GitHub-Hook-Installation-Target-Type: repository

payload={{
  "repository": {{
    "owner": {{
      "login": "testuser"
    }},
    "name": "testrepo"
  }}
}}"#
                ))
            },
            move |response: String| {
                if response.starts_with("HTTP/1.1 200 OK") {
                    if i == repeat - 1 {
                        // This is the last response and we know that we've fired `repeat` jobs at
                        // snare, each taking a bit over 0.05s to execute. So we add a healthy
                        // margin over that time, and sleep for it, hoping that's long enough for
                        // everything to have completed.
                        sleep(std::time::Duration::from_secs(sleep_s));
                    }
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        ));
    }

    run_success(
        &format!(
            r#"listen = "127.0.0.1:0";
maxjobs = 2;
github {{
  match ".*" {{
    queue = {queue_kind};
    cmd = "mktemp -p {tds} {sh_sleep}";
    secret = "secretsecret";
  }}
}}"#
        ),
        &reqs,
    )?;

    Ok(read_dir(&td)?.collect::<Vec<_>>().len())
}

#[test]
fn sequential() {
    assert_eq!(run_queue("sequential", 20, "", 2).unwrap(), 20);
}

#[test]
fn evict() {
    let i = run_queue("evict", 20, "&& sleep 0.4", 4).unwrap();
    // We have a fairly healthy `sleep` above which means most of our requests are likely to be
    // received while the first `cmd` is still running. However, we can't guarantee that, so we are
    // somewhat conservative in the value we can observe. The minimum value we can see is 2 (i.e.
    // only the 1st and 20th jobs were run), but we allow some wiggle room in case some jobs
    // finished before others came in. This allows us a reasonable degree of confidence that we can
    // distinguish "evict" from "parallel".
    if i < 2 || i > 5 {
        panic!("evict test returned {}", i);
    }
}

#[test]
fn parallel() {
    assert_eq!(run_queue("parallel", 20, "", 1,).unwrap(), 20);
}

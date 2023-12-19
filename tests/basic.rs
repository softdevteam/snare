use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use std::{
    convert::{TryFrom, TryInto},
    error::Error,
    fs::read_to_string,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    os::unix::process::ExitStatusExt,
    panic::{catch_unwind, resume_unwind, UnwindSafe},
    process::Stdio,
    thread::sleep,
    time::Duration,
};
use tempfile::Builder;
use wait_timeout::ChildExt;

/// At various points we want to wait for the snare process we've started to do something (e.g.
/// initialise itself): but we have no way of knowing if it's done it or not. The best we can do is
/// to wait for a little bit, hope it does what we want, and then continue with our test. This
/// constant defines how long we wait at any given point. There is no perfect value here: one can
/// always have a box which (perhaps because it's loaded) causes arbitrarily long pauses. So we set
/// a fairly high threshold, hoping that will deal with most reasonable cases, and then cross our
/// fingers!
static SNARE_PAUSE: Duration = Duration::from_secs(1);
/// When we send SIGTERM to a snare instance, what is the maximum time we should wait for the
/// process to exit? We don't expect this maximum time to be reached often, so a fairly high
/// threshold is tolerable, and doing so maximises the chance that we get something useful printed
/// to stdout/stderr.
static SNARE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

fn run<F, G>(cfg: &str, req: F, check_response: G) -> Result<(), Box<dyn Error>>
where
    F: FnOnce(u16) -> Result<String, Box<dyn Error>> + UnwindSafe + 'static,
    G: FnOnce(String) -> Result<(), Box<dyn Error>> + UnwindSafe + 'static,
{
    let mut tc = Builder::new().tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    write!(tc, "{cfg}")?;
    let mut cmd = escargot::CargoBuild::new()
        .bin("snare")
        .current_release()
        .current_target()
        .no_default_features()
        .features("_internal_testing")
        .run()?
        .command();
    let tp = Builder::new().tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    cmd.env("SNARE_DEBUG_PORT_PATH", tp.path().to_str().unwrap());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.args(["-d", "-c", tc.path().to_str().unwrap()]);
    let mut sn = cmd.spawn()?;
    // We want to wait for snare to fully initialise: there is no way of doing that other than
    // waiting and hoping.
    sleep(SNARE_PAUSE);

    // Try as hard as possible not to leave snare processes lurking around after the tests are run,
    // by sending them SIGTERM in as many cases as we reasonably can. Note that `catch_unwind` does
    // not guarantee to catch all panic-y situations, so this can never be perfect.
    let r = catch_unwind(move || {
        let port = read_to_string(tp.path()).unwrap().parse::<u16>().unwrap();

        let req = req(port).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        // We want to wait for snare to execute any actions that might come with the request. Again, we
        // have no way of doing that other than waiting and hoping.
        sleep(SNARE_PAUSE);
        check_response(response).unwrap();
    });

    kill(Pid::from_raw(sn.id().try_into().unwrap()), Signal::SIGTERM).unwrap();
    if r.is_err() {
        let ec = match sn.wait_timeout(SNARE_WAIT_TIMEOUT) {
            Err(e) => e.to_string(),
            Ok(None) => "stalled without exiting".to_owned(),
            Ok(Some(ec)) => ec
                .code()
                .map(|i| i.to_string())
                .unwrap_or_else(|| "<no exit code>".to_owned()),
        };
        let mut stdout = String::new();
        let mut stderr = String::new();
        sn.stdout.as_mut().unwrap().read_to_string(&mut stdout).ok();
        if !stdout.is_empty() {
            stdout.push('\n');
        }
        sn.stderr.as_mut().unwrap().read_to_string(&mut stderr).ok();
        eprintln!(
            "snare child process:\n  Exit status: {ec}\n\n  stdout:\n{stdout}\n  stderr:\n{stderr}"
        );
    }

    match r {
        Ok(()) => (),
        Err(r) => resume_unwind(r),
    }

    match sn.wait_timeout(SNARE_WAIT_TIMEOUT) {
        Err(e) => Err(e.into()),
        Ok(Some(es)) => {
            if let Some(Signal::SIGTERM) = es.signal().map(|x| Signal::try_from(x).unwrap()) {
                Ok(())
            } else {
                Err(format!("Expected successful exit but got '{es:?}'").into())
            }
        }
        Ok(None) => Err("timeout waiting for snare to exit".into()),
    }
}

#[test]
fn minimal_config() -> Result<(), Box<dyn Error>> {
    run(
        r#"listen = "127.0.0.1:0";
github {
}"#,
        |_| Ok(String::new()),
        |_| Ok(()),
    )
}

#[test]
fn full_request() -> Result<(), Box<dyn Error>> {
    // This test checks that snare both responds to, and executes the correct command for, a given
    // (user, repo) pair. It does that by checking that snare executes `touch <tempfile>`.

    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut tp = td.path().to_owned();
    tp.push("t");
    let tps = tp.as_path().to_str().unwrap();
    assert!(!tp.is_file());
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    run(
        &format!(
            r#"listen = "127.0.0.1:0";
github {{
  match ".*" {{
    cmd = "touch {tps}";
    secret = "secretsecret";
  }}
}}"#
        ),
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
        move |response| {
            if response.starts_with("HTTP/1.1 200 OK") {
                assert!(tp.is_file());
                Ok(())
            } else {
                Err(format!("Received HTTP response '{response}'").into())
            }
        },
    )
}

#[test]
fn bad_sha256() -> Result<(), Box<dyn Error>> {
    // Takes the SHA256 from [full_request], alters just the last digit, and checks that snare
    // doesn't execute any commands (so, by proxy, we assume that snare doesn't authenticate the
    // request).

    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut tp = td.path().to_owned();
    tp.push("t");
    let tps = tp.as_path().to_str().unwrap();
    assert!(!tp.is_file());
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    run(
        &format!(
            r#"listen = "127.0.0.1:0";
github {{
  match ".*" {{
    cmd = "touch {tps}";
    secret = "secretsecret";
  }}
}}"#
        ),
        move |port| {
            Ok(format!(
                r#"POST /payload HTTP/1.1
Host: 127.0.0.1:{port}
Content-Length: 104
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256=292e1ce3568fecd98589c71938e19afee9b04b7fe11886d5478d802416bbde67
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
        move |response| {
            if response.starts_with("HTTP/1.1 400") {
                assert!(!tp.is_file());
                Ok(())
            } else {
                Err(format!("Received HTTP response '{response}'").into())
            }
        },
    )
}

#[test]
fn wrong_secret() -> Result<(), Box<dyn Error>> {
    // Takes the example from [full_request], alters the client-side secret, and checks that this
    // causes snare not execute any commands (so, by proxy, we assume that authentication failed).

    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut tp = td.path().to_owned();
    tp.push("t");
    let tps = tp.as_path().to_str().unwrap();
    assert!(!tp.is_file());
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    run(
        &format!(
            r#"listen = "127.0.0.1:0";
github {{
  match ".*" {{
    cmd = "touch {tps}";
    secret = "secretsecretsecret";
  }}
}}"#
        ),
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
        move |response| {
            if response.starts_with("HTTP/1.1 400") {
                assert!(!tp.is_file());
                Ok(())
            } else {
                Err(format!("Received HTTP response '{response}'").into())
            }
        },
    )
}

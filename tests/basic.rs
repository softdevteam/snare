use assert_cmd::prelude::*;
use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use std::{
    convert::{TryFrom, TryInto},
    error::Error,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    os::unix::process::ExitStatusExt,
    panic::{catch_unwind, resume_unwind, UnwindSafe},
    process::{Child, Command, ExitStatus, Stdio},
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

fn run<F, G>(cfg: &str, while_running: F, check_exit: G) -> Result<(), Box<dyn Error>>
where
    F: FnOnce(&Child) -> Result<(), Box<dyn Error>> + UnwindSafe + 'static,
    G: FnOnce(ExitStatus) -> Result<(), Box<dyn Error>> + 'static,
{
    let mut tc = Builder::new().tempfile_in(env!("CARGO_TARGET_TMPDIR"))?;
    write!(tc, "{cfg}")?;
    let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME"))?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.args(["-d", "-c", tc.path().to_str().unwrap()]);
    let mut sn = cmd.spawn()?;
    // We want to wait for snare to fully initialise: there is no way of doing that other than
    // waiting and hoping.
    sleep(SNARE_PAUSE);
    // Try as hard as possible not to leave snare processes lurking around after the tests are run,
    // by sending them SIGTERM in as many cases as we reasonably can. Note that `catch_unwind` does
    // not guarantee to catch all panic-y situations, so this can never be perfect.
    let r = catch_unwind(|| while_running(&sn));
    kill(Pid::from_raw(sn.id().try_into()?), Signal::SIGTERM)?;
    if let Err(_) | Ok(Err(_)) = r {
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
        if stdout.len() > 0 {
            stdout.push('\n');
        }
        sn.stderr.as_mut().unwrap().read_to_string(&mut stderr).ok();
        eprintln!(
            "snare child process:\n  Exit status: {ec}\n\n  stdout:\n{stdout}\n  stderr:\n{stderr}"
        );
    }
    match r {
        Err(r) => resume_unwind(r),
        Ok(Ok(_)) => (),
        Ok(Err(e)) => return Err(e),
    }

    match sn.wait_timeout(SNARE_WAIT_TIMEOUT) {
        Err(e) => Err(e.into()),
        Ok(Some(ec)) => check_exit(ec),
        Ok(None) => Err("timeout waiting for snare to exit".into()),
    }
}

fn exit_success(es: ExitStatus) -> Result<(), Box<dyn Error>> {
    if let Some(Signal::SIGTERM) = es.signal().map(|x| Signal::try_from(x).unwrap()) {
        Ok(())
    } else {
        Err(format!("Expected successful exit but got '{es:?}'").into())
    }
}

fn exit_error(es: ExitStatus) -> Result<(), Box<dyn Error>> {
    if !es.success() {
        Ok(())
    } else {
        Err(format!("Expected unsuccessful exit but got '{es:?}'").into())
    }
}

#[test]
fn minimal_config() -> Result<(), Box<dyn Error>> {
    run(
        r#"listen = "127.0.0.1:28083";
github {
}"#,
        |_| Ok(()),
        exit_success,
    )
}

#[test]
fn bad_config() -> Result<(), Box<dyn Error>> {
    run(r#""#, |_| Ok(()), exit_error)
}

#[test]
fn full_request() -> Result<(), Box<dyn Error>> {
    // This test checks that snare both responds to, and executes the correct command for, a given
    // (user, repo) pair. It does that by checking that snare executes `touch <tempfile>`.

    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut tp = td.path().to_owned();
    tp.push("t");
    let tps = tp.as_path().to_str().unwrap();
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    let req = r#"POST /payload HTTP/1.1
Host: 127.0.0.1:28084
Content-Length: 104
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256=292e1ce3568fecd98589c71938e19afee9b04b7fe11886d5478d802416bbde66
User-Agent: GitHub-Hookshot/044aadd
Content-Type: application/json
X-GitHub-Event: issues
X-GitHub-Hook-ID: 292430182
X-GitHub-Hook-Installation-Target-ID: 79929171
X-GitHub-Hook-Installation-Target-Type: repository

payload={
  "repository": {
    "owner": {
      "login": "testuser"
    },
    "name": "testrepo"
  }
}"#;

    run(
        &format!(
            r#"listen = "127.0.0.1:28084";
github {{
  match ".*" {{
    cmd = "touch {tps}";
    secret = "secretsecret";
  }}
}}"#
        ),
        move |_| {
            assert!(!tp.is_file());
            let mut stream = TcpStream::connect("127.0.0.1:28084")?;
            stream.write(req.as_bytes())?;
            stream.shutdown(Shutdown::Write)?;
            let mut s = String::new();
            stream.read_to_string(&mut s)?;
            if s.starts_with("HTTP/1.1 200 OK") {
                // We want to wait for snare to fully initialise: there is no way of doing that other than
                // waiting and hoping.
                sleep(SNARE_PAUSE);
                assert!(tp.is_file());
                Ok(())
            } else {
                Err(format!("Received HTTP response '{s}'").into())
            }
        },
        exit_success,
    )
}

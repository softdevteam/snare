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
    panic::{catch_unwind, resume_unwind, RefUnwindSafe, UnwindSafe},
    process::{Child, Stdio},
    rc::Rc,
    thread::sleep,
    time::Duration,
};
use tempfile::{Builder, NamedTempFile};
use wait_timeout::ChildExt;

/// At various points we want to wait for the snare process we've started to do something (e.g.
/// initialise itself): but we have no way of knowing if it's done it or not. The best we can do is
/// to wait for a little bit, hope it does what we want, and then continue with our test. This
/// constant defines how long we wait at any given point. There is no perfect value here: one can
/// always have a box which (perhaps because it's loaded) causes arbitrarily long pauses. So we set
/// a fairly high threshold, hoping that will deal with most reasonable cases, and then cross our
/// fingers!
pub static SNARE_PAUSE: Duration = Duration::from_secs(1);
/// When we send SIGTERM to a snare instance, what is the maximum time we should wait for the
/// process to exit? We don't expect this maximum time to be reached often, so a fairly high
/// threshold is tolerable, and doing so maximises the chance that we get something useful printed
/// to stdout/stderr.
static SNARE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[allow(dead_code)]
pub fn run_success<F, G>(cfg: &str, req_check: &[(F, G)]) -> Result<(), Box<dyn Error>>
where
    F: Fn(u16) -> Result<String, Box<dyn Error>> + RefUnwindSafe + UnwindSafe + 'static,
    G: Fn(String) -> Result<(), Box<dyn Error>> + RefUnwindSafe + UnwindSafe + 'static,
{
    let (mut sn, tp) = snare_command(cfg)?;
    match sn.try_wait() {
        Ok(None) => (),
        _ => todo!(),
    }
    let tp = Rc::new(tp);

    for (req, check) in req_check {
        let tp = Rc::clone(&tp);
        let r = catch_unwind(move || {
            let port = read_to_string(tp.path()).unwrap().parse::<u16>().unwrap();

            let req = req(port).unwrap();
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(req.as_bytes()).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();

            check(response).unwrap();
        });

        // Try as hard as possible not to leave snare processes lurking around after the tests are run,
        // by sending them SIGTERM in as many cases as we reasonably can. Note that `catch_unwind` does
        // not guarantee to catch all panic-y situations, so this can never be perfect.
        if let Err(r) = r {
            kill(Pid::from_raw(sn.id().try_into().unwrap()), Signal::SIGTERM).unwrap();
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
            resume_unwind(r);
        }
    }

    kill(Pid::from_raw(sn.id().try_into().unwrap()), Signal::SIGTERM).unwrap();
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

#[allow(dead_code)]
pub fn run_preserver_success(cfg: &str) -> Result<(), Box<dyn Error>> {
    let (mut sn, _tp) = snare_command(cfg)?;
    sleep(SNARE_PAUSE);
    kill(Pid::from_raw(sn.id().try_into().unwrap()), Signal::SIGTERM).unwrap();
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

#[allow(dead_code)]
pub fn run_preserver_error(cfg: &str) -> Result<(), Box<dyn Error>> {
    let (mut sn, _tp) = snare_command(cfg)?;
    sleep(SNARE_PAUSE);
    match sn.wait_timeout(SNARE_WAIT_TIMEOUT) {
        Err(e) => Err(e.into()),
        Ok(Some(es)) => {
            if !es.success() {
                Ok(())
            } else {
                Err("snare exited successfully".into())
            }
        }
        Ok(None) => Err("timeout waiting for snare to exit".into()),
    }
}

fn snare_command(cfg: &str) -> Result<(Child, NamedTempFile), Box<dyn Error>> {
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
    let sn = cmd.spawn()?;
    // We want to wait for snare to fully initialise: there is no way of doing that other than
    // waiting and hoping.
    sleep(SNARE_PAUSE);
    Ok((sn, tp))
}

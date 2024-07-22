use std::{error::Error, path::PathBuf, thread::sleep};
use tempfile::{Builder, TempDir};

mod common;
use common::{run_success, SNARE_PAUSE};

fn cfg(correct_secret: bool) -> Result<(String, TempDir, PathBuf), Box<dyn Error>> {
    let secret = if correct_secret {
        "secretsecret"
    } else {
        "secretsecretsecret"
    };

    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut tp = td.path().to_owned();
    tp.push("t");
    let tps = tp.as_path().to_str().unwrap();

    Ok((
        format!(
            r#"listen = "127.0.0.1:0";
github {{
  match ".*" {{
    cmd = "touch {tps}";
    secret = "{secret}";
  }}
}}"#
        ),
        td,
        tp,
    ))
}

fn req(port: u16, good_sha256: bool, event_type: &str) -> String {
    let sha256 = if good_sha256 {
        "d11297e14fe5286dd68fd58c5e23ea7fb45e60ceff51ec3eb3729400fcbcb4b2"
    } else {
        "d11297e14fe5286dd68fd58c5e23ea7fb45e60ceff51ec3eb3729400fcbcb4b3"
    };

    format!(
        r#"POST /payload HTTP/1.1
Host: 127.0.0.1:{port}
Content-Length: 96
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256={sha256}
User-Agent: GitHub-Hookshot/044aadd
Content-Type: application/json
X-GitHub-Event: {event_type}
X-GitHub-Hook-ID: 292430182
X-GitHub-Hook-Installation-Target-ID: 79929171
X-GitHub-Hook-Installation-Target-Type: repository

{{
  "repository": {{
    "owner": {{
      "login": "testuser"
    }},
    "name": "testrepo"
  }}
}}"#
    )
}

#[test]
fn ping() -> Result<(), Box<dyn Error>> {
    let (cfg, _td, _tp) = cfg(true)?;
    run_success(
        &cfg,
        &[(
            move |port| Ok(req(port, true, "ping")),
            move |response| {
                if response.starts_with("HTTP/1.1 200 OK") {
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
}

#[test]
fn successful_auth() -> Result<(), Box<dyn Error>> {
    // This test checks that snare both responds to, and executes the correct command for, a given
    // (user, repo) pair. It does that by checking that snare executes `touch <tempfile>`.

    let (cfg, _td, tp) = cfg(true)?;
    assert!(!tp.is_file());
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    run_success(
        &cfg,
        &[(
            move |port| Ok(req(port, true, "issues")),
            move |response| {
                if response.starts_with("HTTP/1.1 200 OK") {
                    sleep(SNARE_PAUSE);
                    assert!(tp.is_file());
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
}

#[test]
fn bad_sha256() -> Result<(), Box<dyn Error>> {
    // Takes the SHA256 from [full_request], alters just the last digit, and checks that snare
    // doesn't execute any commands (so, by proxy, we assume that snare doesn't authenticate the
    // request).

    let (cfg, _td, tp) = cfg(true)?;
    assert!(!tp.is_file());
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    run_success(
        &cfg,
        &[(
            move |port| Ok(req(port, false, "issues")),
            move |response| {
                if response.starts_with("HTTP/1.1 401") {
                    sleep(SNARE_PAUSE);
                    assert!(!tp.is_file());
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
}

#[test]
fn wrong_secret() -> Result<(), Box<dyn Error>> {
    // Takes the example from [full_request], alters the client-side secret, and checks that this
    // causes snare not execute any commands (so, by proxy, we assume that authentication failed).

    let (cfg, _td, tp) = cfg(false)?;
    assert!(!tp.is_file());
    // Example secret and payload from
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#testing-the-webhook-payload-validation
    run_success(
        &cfg,
        &[(
            move |port| Ok(req(port, true, "issues")),
            move |response| {
                if response.starts_with("HTTP/1.1 401") {
                    sleep(SNARE_PAUSE);
                    assert!(!tp.is_file());
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
}

use std::error::Error;
use tempfile::Builder;

mod common;
use common::run_success;

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
    run_success(
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
    run_success(
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
    run_success(
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

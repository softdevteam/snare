use std::{fs::read_to_string, thread::sleep};
use tempfile::Builder;

mod common;
use common::{run_success, SNARE_PAUSE};

#[test]
fn multiple() {
    // This tests that when there are multiple actions, the correct one takes effect.

    let td = Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let mut tp1 = td.path().to_owned();
    tp1.push("t1");
    let tp1s = tp1.as_path().to_str().unwrap();
    let mut tp2 = td.path().to_owned();
    tp2.push("t2");
    let tp2s = tp2.as_path().to_str().unwrap();
    let mut tp3 = td.path().to_owned();
    tp3.push("t3");
    let tp3s = tp3.as_path().to_str().unwrap();

    run_success(
        &format!(
            r#"listen = "127.0.0.1:0";
github {{
  // Should match
  match "*" {{
    secret = "secretsecret";
  }}
  // Shouldn't match
  match "testuser/testrep" {{
    cmd = "touch {tp1s}";
    secret = "secretsecretsecret";
  }}
  // Should match but will be overridden by the following entry
  match "testuser/testrepo" {{
    cmd = "touch {tp2s}";
  }}
  // Should match and override the previous entry
  match "testuser/testrepo" {{
    cmd = "touch {tp3s}";
  }}
}}"#
        ),
        &[(
            move |port| {
                Ok(format!(
                    r#"POST /payload HTTP/1.1
Host: 127.0.0.1:{port}
Content-Length: 96
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256=d11297e14fe5286dd68fd58c5e23ea7fb45e60ceff51ec3eb3729400fcbcb4b2
User-Agent: GitHub-Hookshot/044aadd
Content-Type: application/json
X-GitHub-Event: issues
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
                ))
            },
            move |response: String| {
                if response.starts_with("HTTP/1.1 200 OK") {
                    sleep(SNARE_PAUSE);
                    assert!(!tp1.is_file());
                    assert!(!tp2.is_file());
                    assert!(tp3.is_file());
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
    .unwrap();
}

#[test]
fn errorcmd() {
    // This tests both large stdout/stderr output from `cmd` as well as that `errorcmd` works.

    let td = Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let mut tp1 = td.path().to_owned();
    tp1.push("t1");
    let tp1s = tp1.as_path().to_str().unwrap();
    let mut tp2 = td.path().to_owned();
    tp2.push("t2");
    let tp2s = tp2.as_path().to_str().unwrap();

    run_success(
        &format!(
            r#"listen = "127.0.0.1:0";
github {{
  match ".*" {{
    cmd = "dd if=/dev/zero bs=1k count=256 status=none && dd if=/dev/zero of=/dev/stderr bs=1k count=256 status=none && exit 1";
    errorcmd = "echo %x %? > {tp1s} && cp %s {tp2s}";
    secret = "secretsecret";
  }}
}}"#
        ),
        &[(
            move |port| {
                Ok(format!(
                    r#"POST /payload HTTP/1.1
Host: 127.0.0.1:{port}
Content-Length: 96
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256=d11297e14fe5286dd68fd58c5e23ea7fb45e60ceff51ec3eb3729400fcbcb4b2
User-Agent: GitHub-Hookshot/044aadd
Content-Type: application/json
X-GitHub-Event: issues
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
                ))
            },
            move |response: String| {
                if response.starts_with("HTTP/1.1 200 OK") {
                    sleep(SNARE_PAUSE);
                    assert_eq!(read_to_string(&tp1).unwrap().trim(), "status 1");
                    assert_eq!(read_to_string(&tp2).unwrap().len(), 2 * 256 * 1024);
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    ).unwrap();
}

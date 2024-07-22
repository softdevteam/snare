use std::error::Error;

mod common;
use common::run_success;

#[test]
fn content_type_json() -> Result<(), Box<dyn Error>> {
    run_success(
        r#"
            listen = "127.0.0.1:0";
            github {
                match ".*" {
                    cmd = "true";
                    secret = "secretsecret";
                }
            }
        "#,
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
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
}

#[test]
fn content_type_url_encoded() -> Result<(), Box<dyn Error>> {
    run_success(
        r#"
            listen = "127.0.0.1:0";
            github {
                match ".*" {
                    cmd = "true";
                    secret = "secretsecret";
                }
            }
        "#,
        &[(
            move |port| {
                Ok(format!(
                    r#"POST /payload HTTP/1.1
Host: 127.0.0.1:{port}
Content-Length: 104
X-GitHub-Delivery: 72d3162e-cc78-11e3-81ab-4c9367dc0958
X-Hub-Signature-256: sha256=292e1ce3568fecd98589c71938e19afee9b04b7fe11886d5478d802416bbde66
User-Agent: GitHub-Hookshot/044aadd
Content-Type: application/x-www-form-urlencoded
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
                    Ok(())
                } else {
                    Err(format!("Received HTTP response '{response}'").into())
                }
            },
        )],
    )
}

use std::error::Error;

use tempfile::Builder;

mod common;
use common::run_unix_success;

#[test]
fn accepts_http_requests() -> Result<(), Box<dyn Error>> {
    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let socket_path = td.path().join("snare.sock");
    let cfg = format!(
        r#"
            listen = "unix:{}";
            github {{
                match ".*" {{
                    cmd = "true";
                }}
            }}
        "#,
        socket_path.display()
    );
    let body = r#"{
  "repository": {
    "owner": {
      "login": "testuser"
    },
    "name": "testrepo"
  }
}"#;
    let request = format!(
        "POST /payload HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Length: {}\r\n\
         Content-Type: application/json\r\n\
         X-GitHub-Event: ping\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

    run_unix_success(&cfg, &socket_path, &request)
}

use std::error::Error;

use tempfile::Builder;

mod common;
use common::{run_preserver_error, run_preserver_success};

#[test]
fn empty_config() -> Result<(), Box<dyn Error>> {
    run_preserver_error(r#""#)
}

#[test]
fn minimal_config() -> Result<(), Box<dyn Error>> {
    run_preserver_success(
        r#"listen = "127.0.0.1:0";
github {
}"#,
    )
}

#[test]
fn bad_minimal_config() -> Result<(), Box<dyn Error>> {
    run_preserver_error(
        r#"listen = "127.0.0.1:0";
github {
    xyz;
}"#,
    )
}

#[test]
fn relative_socket_path_is_valid() -> Result<(), Box<dyn Error>> {
    let td = Builder::new().tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let socket_path = td.path().strip_prefix("/")?.join("snare.sock");
    run_preserver_success(&format!(
        r#"listen = "unix:{}";
github {{
}}"#,
        socket_path.display()
    ))
}

#[test]
fn empty_socket_path_is_invalid() -> Result<(), Box<dyn Error>> {
    run_preserver_error(
        r#"listen = "unix:";
github {
}"#,
    )
}

#[test]
fn socket_path_without_unix_prefix_is_invalid() -> Result<(), Box<dyn Error>> {
    run_preserver_error(
        r#"listen = "/run/snare.sock";
github {
}"#,
    )
}

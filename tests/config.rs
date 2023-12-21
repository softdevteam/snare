use std::error::Error;

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

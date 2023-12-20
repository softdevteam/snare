use std::error::Error;

mod common;
use common::run;

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

//! `controlroom-fake-agent`: a scriptable stand-in for `pi --mode rpc`.
//!
//! ```text
//! controlroom-fake-agent --mode rpc --scenario scenario.json --gate-dir /tmp/gates
//! controlroom-fake-agent --mode rpc --scenario-json '{"replies":[{"content":[{"type":"text","text":"hi"}]}]}'
//! ```
//!
//! See the library docs for the flags and [`controlroom_fake_agent::Scenario`]
//! for the script format.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match controlroom_fake_agent::config_from_args(&args) {
        Ok(config) => controlroom_fake_agent::serve(config).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(code) => {
            // Leave at once, like pi: running tasks do not get to finish.
            std::process::exit(code);
        }
        Err(err) => {
            eprintln!("fake-agent: {err:#}");
            ExitCode::FAILURE
        }
    }
}

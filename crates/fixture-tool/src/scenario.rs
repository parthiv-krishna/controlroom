//! Running one recording scenario: a mock model script plus a driver that
//! talks to one or more pi processes.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use serde_json::json;

use crate::env::PiEnv;
use crate::mock_model::{MockModel, Reply};
use crate::record::Record;
use crate::rig::{PiProcess, WAIT};

/// Context handed to a scenario's driver.
pub struct Ctx {
    pub env: PiEnv,
    pub mock: MockModel,
    /// Finished recordings, in order (one per pi process).
    pub recordings: Vec<Vec<Record>>,
}

impl Ctx {
    /// Spawn pi with the standard flags plus `extra`.
    pub fn spawn(&self, extra: &[&str]) -> Result<PiProcess> {
        let ext = self.env.extension().display().to_string();
        let mut args: Vec<String> = vec![
            "--no-extensions".into(),
            "--no-skills".into(),
            "--no-prompt-templates".into(),
            "--no-themes".into(),
            "-e".into(),
            ext,
        ];
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        let env = json!({
            "fixture": "env",
            "root": self.env.root().display().to_string(),
            "baseUrl": self.mock.base_url(),
            "args": args,
        });
        PiProcess::spawn(&self.env, &args, &env)
    }

    /// Store a finished process's transcript.
    pub fn keep(&mut self, pi: &PiProcess) {
        self.recordings.push(pi.records());
    }

    /// Wait until the model reaches `gate`, then release it and annotate the
    /// transcript, so replays know when the gate opened.
    pub async fn release(&self, pi: &PiProcess, gate: &str) -> Result<()> {
        self.mock.wait_reached(gate, WAIT).await?;
        pi.meta(json!({"fixture": "release_gate", "gate": gate}));
        self.mock.release(gate);
        Ok(())
    }
}

/// A scenario's driver.
pub type Driver = for<'a> fn(&'a mut Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

/// One recording scenario.
pub struct Scenario {
    /// Fixture file stem.
    pub name: &'static str,
    /// One line for the README index.
    pub about: &'static str,
    /// Model replies, consumed in request order.
    pub replies: fn() -> Vec<Reply>,
    pub driver: Driver,
}

/// Run `scenario` and return its recordings with their file names.
pub async fn run(scenario: &Scenario) -> Result<Vec<(String, Vec<Record>)>> {
    let mock = MockModel::start((scenario.replies)()).await?;
    let env = PiEnv::new(&mock.base_url())?;
    let mut ctx = Ctx {
        env,
        mock,
        recordings: Vec::new(),
    };
    (scenario.driver)(&mut ctx).await?;
    if ctx.mock.remaining() > 0 {
        anyhow::bail!(
            "{}: {} scripted model replies were never requested",
            scenario.name,
            ctx.mock.remaining()
        );
    }
    let count = ctx.recordings.len();
    Ok(ctx
        .recordings
        .into_iter()
        .enumerate()
        .map(|(i, records)| {
            let name = if count == 1 {
                format!("{}.jsonl", scenario.name)
            } else {
                format!("{}.{}.jsonl", scenario.name, i + 1)
            };
            (name, records)
        })
        .collect())
}

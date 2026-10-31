//! Running one recording scenario: a mock model script plus a driver that
//! talks to one or more pi processes.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use crate::env::PiEnv;
use crate::fixture::{Fixture, recorder_meta};
use crate::mock_model::{MockModel, Reply};
use crate::rig::{PiProcess, WAIT};

/// Context handed to a scenario's driver.
pub struct Ctx {
    pub env: PiEnv,
    pub mock: MockModel,
    /// The scenario name, used as the transcripts' `meta.id`.
    name: &'static str,
    /// Processes spawned so far.
    spawned: usize,
    /// Finished recordings, in order (one per pi process).
    recordings: Vec<Fixture>,
}

impl Ctx {
    /// Spawn pi with the standard flags plus `extra`.
    pub fn spawn(&mut self, extra: &[&str]) -> Result<PiProcess> {
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
        self.spawned += 1;
        let transcript = self
            .env
            .root()
            .join(format!("transcripts/{}.jsonl", self.spawned));
        let root = self.env.root().display().to_string();
        let meta = recorder_meta(self.name, &root, &self.mock.base_url());
        PiProcess::spawn(&self.env, &args, &transcript, meta)
    }

    /// Store a finished process's recording.
    pub fn keep(&mut self, pi: &PiProcess) -> Result<()> {
        self.recordings.push(pi.fixture()?);
        Ok(())
    }

    /// Wait until the model reaches `gate`, then note the release (so replays
    /// know when the gate opened) and release it.
    pub async fn release(&self, pi: &PiProcess, gate: &str) -> Result<()> {
        self.mock.wait_reached(gate, WAIT).await?;
        pi.released_gate(gate);
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
pub async fn run(scenario: &Scenario) -> Result<Vec<(String, Fixture)>> {
    let mock = MockModel::start((scenario.replies)()).await?;
    let env = PiEnv::new(&mock.base_url())?;
    let mut ctx = Ctx {
        env,
        mock,
        name: scenario.name,
        spawned: 0,
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
        .map(|(i, fixture)| {
            let name = if count == 1 {
                format!("{}.jsonl", scenario.name)
            } else {
                format!("{}.{}.jsonl", scenario.name, i + 1)
            };
            (name, fixture)
        })
        .collect())
}

//! `controlroom-fixture-tool`: record real pi RPC transcripts against a
//! local mock model, and sanitize them into committed fixtures.
//!
//! ```text
//! controlroom-fixture-tool record --raw-dir /tmp/raw [--only plain-text]
//! controlroom-fixture-tool sanitize /tmp/raw fixtures/pi-rpc/1.2.0
//! controlroom-fixture-tool list
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use controlroom_fixture_tool::record::{self, Dir, Record};
use controlroom_fixture_tool::sanitize::Sanitizer;
use controlroom_fixture_tool::{scenario, scenarios};

#[derive(Debug, Parser)]
#[command(name = "controlroom-fixture-tool", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run scenarios against real pi (`$CONTROLROOM_PI` or `pi` on PATH) and
    /// write raw, unsanitized transcripts.
    Record {
        #[arg(long)]
        raw_dir: PathBuf,
        /// Only these scenarios (repeatable).
        #[arg(long)]
        only: Vec<String>,
    },
    /// Sanitize every `*.jsonl` in `raw_dir` into `out_dir`. Files of one
    /// scenario (`name.jsonl`, `name.N.jsonl`) share id maps.
    Sanitize { raw_dir: PathBuf, out_dir: PathBuf },
    /// List the scenarios.
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Record { raw_dir, only } => record_scenarios(&raw_dir, &only).await,
        Cmd::Sanitize { raw_dir, out_dir } => sanitize_dir(&raw_dir, &out_dir),
        Cmd::List => {
            for scenario in scenarios::all() {
                println!("{:<26} {}", scenario.name, scenario.about);
            }
            Ok(())
        }
    }
}

async fn record_scenarios(raw_dir: &Path, only: &[String]) -> Result<()> {
    let all = scenarios::all();
    let selected: Vec<_> = all
        .iter()
        .filter(|s| only.is_empty() || only.iter().any(|o| o == s.name))
        .collect();
    if selected.is_empty() {
        bail!("no scenario matches {only:?}");
    }
    for scenario in selected {
        eprintln!("recording {}", scenario.name);
        for (name, records) in scenario::run(scenario).await? {
            record::write_jsonl(&raw_dir.join(name), &records)?;
        }
    }
    Ok(())
}

fn sanitize_dir(raw_dir: &Path, out_dir: &Path) -> Result<()> {
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in
        std::fs::read_dir(raw_dir).with_context(|| format!("reading {}", raw_dir.display()))?
    {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".jsonl") {
            let scenario = stem.split('.').next().unwrap_or(stem).to_owned();
            groups.entry(scenario).or_default().push(path);
        }
    }
    let sanitizer = Sanitizer::for_this_machine();
    for paths in groups.values_mut() {
        paths.sort();
        let raw: Vec<Vec<Record>> = paths
            .iter()
            .map(|p| record::read_jsonl(p))
            .collect::<Result<_>>()?;
        for (path, (before, after)) in paths.iter().zip(raw.iter().zip(sanitizer.scenario(&raw))) {
            check_json_preserved(path, before, &after)?;
            let name = path.file_name().context("file name")?;
            record::write_jsonl(&out_dir.join(name), &after)?;
        }
    }
    Ok(())
}

/// Every `in`/`out`/`meta` line that was JSON must still be JSON.
fn check_json_preserved(path: &Path, before: &[Record], after: &[Record]) -> Result<()> {
    let is_json = |raw: &str| serde_json::from_str::<serde_json::Value>(raw).is_ok();
    for (b, a) in before.iter().zip(after) {
        if matches!(b.dir, Dir::In | Dir::Out | Dir::Meta) && is_json(&b.raw) && !is_json(&a.raw) {
            bail!(
                "{}: seq {} is no longer JSON after sanitizing",
                path.display(),
                b.seq
            );
        }
    }
    Ok(())
}

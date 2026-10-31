//! An isolated pi environment in a temp dir: `HOME`, XDG dirs, an agent dir
//! with a `models.json` pointing at the mock model, a test extension and a
//! small project directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

/// The mock provider name in `models.json`.
pub const PROVIDER: &str = "mock";
/// The default mock model id.
pub const MODEL: &str = "mock-model";
/// A second model, for `set_model`.
pub const MODEL_B: &str = "mock-model-b";
/// `maxTokens` of the mock models; a length stop reports this many tokens.
pub const MAX_TOKENS: u64 = 4096;

/// The test extension, loaded with `-e`.
const EXTENSION: &str = include_str!("../../../fixtures/pi-rpc/extension/fixture-ext.ts");

/// Files of the scratch project pi works in.
const PROJECT_FILES: &[(&str, &str)] = &[
    ("notes.txt", "alpha\nbeta\ngamma\n"),
    ("src/main.txt", "fn main() {}\n"),
];

/// A temp dir laid out as `home/`, `xdg/{config,data,state,cache}/`,
/// `agent/` (`PI_CODING_AGENT_DIR`) and `work/` (pi's cwd).
pub struct PiEnv {
    dir: tempfile::TempDir,
}

impl PiEnv {
    /// Create the layout and write the config for a mock at `base_url`.
    pub fn new(base_url: &str) -> Result<Self> {
        // A fixed prefix lets the sanitizer recognize the temp dir anywhere.
        let dir = tempfile::Builder::new()
            .prefix("cr-fixture-")
            .tempdir()
            .context("creating the temp dir")?;
        let env = Self { dir };
        for sub in ["home", "xdg/config", "xdg/data", "xdg/state", "xdg/cache"] {
            std::fs::create_dir_all(env.root().join(sub))?;
        }
        env.write_agent_dir(base_url)?;
        env.reset_project()?;
        Ok(env)
    }

    /// The temp dir.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// `PI_CODING_AGENT_DIR`.
    pub fn agent_dir(&self) -> PathBuf {
        self.root().join("agent")
    }

    /// pi's working directory.
    pub fn work_dir(&self) -> PathBuf {
        self.root().join("work")
    }

    /// The test extension's path.
    pub fn extension(&self) -> PathBuf {
        self.root().join("ext/fixture-ext.ts")
    }

    /// Environment variables for pi. The parent environment is cleared, so
    /// nothing from the user's setup leaks in.
    pub fn vars(&self) -> Vec<(String, String)> {
        let root = self.root();
        let path = |p: &str| root.join(p).display().to_string();
        vec![
            ("HOME".into(), path("home")),
            ("XDG_CONFIG_HOME".into(), path("xdg/config")),
            ("XDG_DATA_HOME".into(), path("xdg/data")),
            ("XDG_STATE_HOME".into(), path("xdg/state")),
            ("XDG_CACHE_HOME".into(), path("xdg/cache")),
            (
                "PI_CODING_AGENT_DIR".into(),
                self.agent_dir().display().to_string(),
            ),
            ("PI_OFFLINE".into(), "1".into()),
            ("PI_SKIP_VERSION_CHECK".into(), "1".into()),
            ("PI_TELEMETRY".into(), "0".into()),
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("LANG".into(), "C.UTF-8".into()),
            ("TERM".into(), "dumb".into()),
            ("USER".into(), "alice".into()),
        ]
    }

    /// Restore the scratch project to its initial files.
    pub fn reset_project(&self) -> Result<()> {
        let work = self.work_dir();
        if work.exists() {
            std::fs::remove_dir_all(&work)?;
        }
        for (name, text) in PROJECT_FILES {
            write(&work.join(name), text)?;
        }
        Ok(())
    }

    fn write_agent_dir(&self, base_url: &str) -> Result<()> {
        let model = |id: &str, name: &str| {
            json!({
                "id": id,
                "name": name,
                "reasoning": true,
                "input": ["text"],
                "contextWindow": 128_000,
                "maxTokens": MAX_TOKENS,
                "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.0, "cacheWrite": 0.0}
            })
        };
        let models = json!({
            "providers": {
                PROVIDER: {
                    "baseUrl": base_url,
                    "api": "openai-completions",
                    "apiKey": "mock-key",
                    "models": [model(MODEL, "Mock Model"), model(MODEL_B, "Mock Model B")]
                }
            }
        });
        let settings = json!({
            "defaultProvider": PROVIDER,
            "defaultModel": MODEL,
            "defaultThinkingLevel": "medium",
            "quietStartup": true,
            "retry": {"enabled": true, "maxRetries": 2, "baseDelayMs": 20},
            "compaction": {"enabled": false, "keepRecentTokens": 10}
        });
        let agent = self.agent_dir();
        write(
            &agent.join("models.json"),
            &serde_json::to_string_pretty(&models)?,
        )?;
        write(
            &agent.join("settings.json"),
            &serde_json::to_string_pretty(&settings)?,
        )?;
        write(&self.extension(), EXTENSION)
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

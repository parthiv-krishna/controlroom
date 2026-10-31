//! Gate steps: deterministic barriers between a test and the fake agent.
//!
//! Gates live in a directory shared with the test (`--gate-dir` or
//! `$CONTROLROOM_FAKE_GATES`). When the agent reaches gate `name` it creates
//! `<dir>/<name>.reached`; it continues once `<dir>/<name>.release` exists.
//! Both sides poll the file system, so a test never sleeps for a guessed
//! duration: it waits for `reached`, acts, then releases.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Polling interval for gate files.
const POLL: Duration = Duration::from_millis(5);

/// A gate directory.
#[derive(Debug, Clone)]
pub struct Gates {
    dir: PathBuf,
}

impl Gates {
    /// Use (and create) `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn reached_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.reached"))
    }

    fn release_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.release"))
    }

    /// Test side: let the agent pass `name` (now or when it gets there).
    pub fn release(&self, name: &str) -> Result<()> {
        let path = self.release_path(name);
        std::fs::write(&path, b"").with_context(|| format!("writing {}", path.display()))
    }

    /// Test side: whether the agent has reached `name`.
    pub fn is_reached(&self, name: &str) -> bool {
        self.reached_path(name).exists()
    }

    /// Test side: wait until the agent reaches `name`.
    pub async fn wait_reached(&self, name: &str, timeout: Duration) -> Result<()> {
        let path = self.reached_path(name);
        let wait = async {
            while !path.exists() {
                tokio::time::sleep(POLL).await;
            }
        };
        if tokio::time::timeout(timeout, wait).await.is_err() {
            bail!("timed out waiting for the agent to reach gate {name}");
        }
        Ok(())
    }

    /// Agent side: mark `name` reached and wait for its release.
    pub async fn pass(&self, name: &str) -> Result<()> {
        let reached = self.reached_path(name);
        std::fs::write(&reached, b"").with_context(|| format!("writing {}", reached.display()))?;
        let release = self.release_path(name);
        while !release.exists() {
            tokio::time::sleep(POLL).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pass_waits_for_release() {
        let dir = tempfile::tempdir().unwrap();
        let gates = Gates::new(dir.path()).unwrap();
        let agent = gates.clone();
        let task = tokio::spawn(async move { agent.pass("g1").await });
        gates
            .wait_reached("g1", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!task.is_finished());
        gates.release("g1").unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn early_release_passes_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let gates = Gates::new(dir.path()).unwrap();
        gates.release("g2").unwrap();
        gates.pass("g2").await.unwrap();
        assert!(gates.is_reached("g2"));
    }
}

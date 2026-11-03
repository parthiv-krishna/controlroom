//! `agents.toml`: the agent registry (prototype plan §6).
//!
//! ```toml
//! [agents.pi]
//! command = "pi"   # run as `$SHELL -lc 'exec <command> <driver args>'`
//! driver = "pi"    # optional; the only driver is pi
//! ```
//!
//! Entries replace the built-in entry of the same name; other built-ins remain. The built-in
//! `pi` entry runs the `pi` found by the login shell. Execution hosts point it at the
//! controlroom-pi bundle's launcher (spec §14), which pins pi and loads only @controlroom/pi and
//! the keep-list:
//!
//! ```toml
//! [agents.pi]
//! command = "controlroom-pi --keep /nix/store/…-provider-extension --keep npm:pi-lens"
//! ```
//!
//! The program must be in a `PATH` directory: sandboxed sessions (nono) can't start it otherwise
//! (`doctor` checks this).

use std::collections::BTreeMap;

use controlroom_protocol::session::DriverKind;
use serde::Deserialize;

use super::{Loaded, load_toml};
use crate::dirs::Dirs;

/// One configured agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    /// A shell command line (program and fixed arguments); the driver's arguments are appended.
    pub command: String,
    /// The driver that speaks to it.
    #[serde(default = "default_driver", deserialize_with = "driver")]
    pub driver: DriverKind,
}

fn default_driver() -> DriverKind {
    DriverKind::Pi
}

/// Only `pi` is accepted: the fake driver never runs a process.
fn driver<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DriverKind, D::Error> {
    match String::deserialize(d)?.as_str() {
        "pi" => Ok(DriverKind::Pi),
        other => Err(serde::de::Error::custom(format!(
            "unknown driver {other:?} (expected \"pi\")"
        ))),
    }
}

/// The registry: built-ins merged with `agents.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agents(pub BTreeMap<String, AgentEntry>);

impl Default for Agents {
    fn default() -> Self {
        Self(BTreeMap::from([(
            "pi".to_owned(),
            AgentEntry {
                command: "pi".to_owned(),
                driver: DriverKind::Pi,
            },
        )]))
    }
}

impl<'de> Deserialize<'de> for Agents {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct File {
            #[serde(default)]
            agents: BTreeMap<String, AgentEntry>,
        }
        let file = File::deserialize(d)?;
        let mut agents = Agents::default();
        agents.0.extend(file.agents);
        Ok(agents)
    }
}

/// Loads `agents.toml`.
pub fn load(dirs: &Dirs) -> Loaded<Agents> {
    load_toml(&dirs.agents_toml())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Agents, toml::de::Error> {
        toml::from_str(s)
    }

    #[test]
    fn defaults_and_overrides() {
        let agents = parse("").unwrap();
        assert_eq!(agents, Agents::default());
        assert_eq!(agents.0["pi"].command, "pi");

        let agents = parse(
            r#"
[agents.pi]
command = "/opt/controlroom-pi/bin/pi --offline"
[agents.second]
command = "pi"
driver = "pi"
"#,
        )
        .unwrap();
        assert_eq!(
            agents.0["pi"].command,
            "/opt/controlroom-pi/bin/pi --offline"
        );
        assert_eq!(agents.0["second"].driver, DriverKind::Pi);
        assert_eq!(agents.0.len(), 2);
    }

    #[test]
    fn rejects_unknown_drivers_and_fields() {
        assert!(parse("[agents.x]\ncommand = \"x\"\ndriver = \"acp\"\n").is_err());
        assert!(parse("[agents.x]\ncommand = \"x\"\nextra = 1\n").is_err());
        assert!(parse("[agents.x]\n").is_err());
    }
}

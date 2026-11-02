//! Agent dependencies: whether each `agents.toml` command resolves through the login shell. A
//! missing program marks that agent unavailable on this machine, with the reason.

use controlroom_protocol::daemon::{AgentInfo, AgentStatus};
use controlroom_sandbox::AbsPath;

use crate::config::agents::Agents;
use crate::shell::{command_program, resolve_program};

/// Checks every configured agent, in parallel (login shells can be slow).
pub fn check_agents(shell: &AbsPath, agents: &Agents) -> Vec<AgentInfo> {
    std::thread::scope(|scope| {
        let checks: Vec<_> = agents
            .0
            .iter()
            .map(|(name, entry)| {
                scope.spawn(move || {
                    let (status, path, detail) = resolve(shell, &entry.command);
                    AgentInfo {
                        agent: name.clone(),
                        driver: entry.driver,
                        command: entry.command.clone(),
                        status,
                        path: path.map(Into::into),
                        detail,
                    }
                })
            })
            .collect();
        checks
            .into_iter()
            .map(|c| c.join().expect("agent checks don't panic"))
            .collect()
    })
}

fn resolve(shell: &AbsPath, command: &str) -> (AgentStatus, Option<String>, String) {
    let Some(program) = command_program(command) else {
        return (AgentStatus::Missing, None, "empty command".into());
    };
    match resolve_program(shell, program) {
        Ok(Some(path)) => (
            AgentStatus::Ok,
            Some(path.clone()),
            format!("found at {path}"),
        ),
        Ok(None) => (
            AgentStatus::Missing,
            None,
            format!("`{program}` not found by the login shell ({shell} -lc)"),
        ),
        Err(e) => (AgentStatus::Unknown, None, e.describe(shell.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use controlroom_protocol::session::DriverKind;

    use super::*;
    use crate::config::agents::AgentEntry;

    #[test]
    fn resolves_through_the_shell() {
        let entry = |command: &str| AgentEntry {
            command: command.into(),
            driver: DriverKind::Pi,
        };
        let agents = Agents(BTreeMap::from([
            ("found".to_owned(), entry("sh -c true")),
            (
                "missing".to_owned(),
                entry("controlroom-no-such-agent --mode rpc"),
            ),
            ("empty".to_owned(), entry("  ")),
        ]));
        let infos = check_agents(&AbsPath::new("/bin/sh").unwrap(), &agents);
        let by_name = |n: &str| infos.iter().find(|i| i.agent == n).unwrap();
        assert_eq!(by_name("found").status, AgentStatus::Ok);
        assert!(by_name("found").path.is_some());
        assert_eq!(by_name("missing").status, AgentStatus::Missing);
        assert!(
            by_name("missing")
                .detail
                .contains("controlroom-no-such-agent")
        );
        assert_eq!(by_name("empty").status, AgentStatus::Missing);
    }
}

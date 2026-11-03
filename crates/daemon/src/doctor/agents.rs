//! Agent dependencies: whether each `agents.toml` command resolves through the login shell to a
//! program in one of controlroomd's `PATH` directories. A missing program marks that agent
//! unavailable on this machine, with the reason and the fix.
//!
//! The `PATH` requirement comes from nono: with command mediation (on whenever a mediated
//! command such as `git` is on `PATH`), a sandboxed session may only execute the programs it
//! finds in nono's `PATH` directories, which is controlroomd's `PATH`. An agent configured by
//! absolute path elsewhere (a bare store path, say) fails to start with exit status 126.

use std::path::Path;

use controlroom_protocol::daemon::{AgentInfo, AgentStatus};
use controlroom_sandbox::AbsPath;

use crate::config::agents::Agents;
use crate::shell::{command_program, resolve_program};

/// Checks every configured agent, in parallel (login shells can be slow). `path_var` is the
/// `PATH` sandboxed launches get (controlroomd's own).
pub fn check_agents(shell: &AbsPath, path_var: &str, agents: &Agents) -> Vec<AgentInfo> {
    std::thread::scope(|scope| {
        let checks: Vec<_> = agents
            .0
            .iter()
            .map(|(name, entry)| {
                scope.spawn(move || {
                    let (status, path, detail) = resolve(shell, path_var, &entry.command);
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

fn resolve(
    shell: &AbsPath,
    path_var: &str,
    command: &str,
) -> (AgentStatus, Option<String>, String) {
    let Some(program) = command_program(command) else {
        return (AgentStatus::Missing, None, "empty command".into());
    };
    let path = match resolve_program(shell, program) {
        Ok(Some(path)) => path,
        Ok(None) => {
            return (
                AgentStatus::Missing,
                None,
                format!("`{program}` not found by the login shell ({shell} -lc)"),
            );
        }
        Err(e) => return (AgentStatus::Unknown, None, e.describe(shell.as_str())),
    };
    match on_path(Path::new(&path), path_var) {
        PathCheck::InDir(dir) => (
            AgentStatus::Ok,
            Some(path.clone()),
            format!("found at {path} (PATH directory {dir})"),
        ),
        PathCheck::NotAProgram => (
            AgentStatus::Missing,
            None,
            format!(
                "`{program}` is a shell alias, function or builtin ({path}); \
                 set `command` in agents.toml to a program on PATH"
            ),
        ),
        PathCheck::Outside => {
            let name = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                AgentStatus::Missing,
                Some(path.clone()),
                format!(
                    "{path} is not in a PATH directory of controlroomd, and sandboxed sessions \
                     (nono) can only start programs from PATH directories; install it into a \
                     profile on PATH (e.g. home.packages) and set `command = \"{name} …\"` in \
                     agents.toml"
                ),
            )
        }
    }
}

/// Where a resolved program stands relative to `PATH`.
#[derive(Debug, PartialEq, Eq)]
enum PathCheck {
    /// The same file is reachable through this `PATH` directory.
    InDir(String),
    /// Not a path at all (alias, function, builtin).
    NotAProgram,
    /// A file, but in no `PATH` directory.
    Outside,
}

/// Whether `program` (as printed by `command -v`) is, file for file, an entry of a `path_var`
/// directory. nono compares canonical files, so symlinked profile entries count.
fn on_path(program: &Path, path_var: &str) -> PathCheck {
    let (Some(name), true) = (program.file_name(), program.is_absolute()) else {
        return PathCheck::NotAProgram;
    };
    let Ok(real) = std::fs::canonicalize(program) else {
        return PathCheck::Outside;
    };
    path_var
        .split(':')
        .filter(|dir| dir.starts_with('/'))
        .find(|dir| std::fs::canonicalize(Path::new(dir).join(name)).is_ok_and(|p| p == real))
        .map_or(PathCheck::Outside, |dir| PathCheck::InDir(dir.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use controlroom_protocol::session::DriverKind;

    use super::*;
    use crate::config::agents::AgentEntry;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn programs_must_be_in_a_path_directory() {
        let t = tempfile::tempdir().unwrap();
        let store = t.path().join("store/aaaa-pi/bin/pi");
        executable(&store);
        let profile = t.path().join("profile/bin");
        std::fs::create_dir_all(&profile).unwrap();
        symlink(&store, profile.join("pi")).unwrap();
        let path_var = format!("relative:{}", profile.display());

        // Through a profile link, or by the store path the link points to: the same file.
        assert_eq!(
            on_path(&profile.join("pi"), &path_var),
            PathCheck::InDir(profile.display().to_string())
        );
        assert_eq!(
            on_path(&store, &path_var),
            PathCheck::InDir(profile.display().to_string())
        );
        // A different copy elsewhere is not on PATH.
        let other = t.path().join("elsewhere/pi");
        executable(&other);
        assert_eq!(on_path(&other, &path_var), PathCheck::Outside);
        assert_eq!(on_path(Path::new("pi"), &path_var), PathCheck::NotAProgram);
    }

    #[test]
    fn resolves_through_the_shell_and_checks_path() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        executable(&bin.join("agent"));
        let outside = t.path().join("outside/agent");
        executable(&outside);
        let entry = |command: String| AgentEntry {
            command,
            driver: DriverKind::Pi,
        };
        let agents = Agents(BTreeMap::from([
            (
                "found".to_owned(),
                entry(format!("{}/agent --mode rpc", bin.display())),
            ),
            (
                "outside".to_owned(),
                entry(format!("{} --mode rpc", outside.display())),
            ),
            ("builtin".to_owned(), entry("true".into())),
            (
                "missing".to_owned(),
                entry("controlroom-no-such-agent --mode rpc".into()),
            ),
            ("empty".to_owned(), entry("  ".into())),
        ]));
        let path_var = bin.display().to_string();
        let infos = check_agents(&AbsPath::new("/bin/sh").unwrap(), &path_var, &agents);
        let by_name = |n: &str| infos.iter().find(|i| i.agent == n).unwrap();
        assert_eq!(by_name("found").status, AgentStatus::Ok);
        assert!(by_name("found").path.is_some());
        let outside = by_name("outside");
        assert_eq!(outside.status, AgentStatus::Missing);
        assert!(
            outside.detail.contains("not in a PATH directory"),
            "{}",
            outside.detail
        );
        assert!(
            outside.detail.contains("command = \"agent …\""),
            "{}",
            outside.detail
        );
        assert_eq!(by_name("builtin").status, AgentStatus::Missing);
        assert!(
            by_name("builtin")
                .detail
                .contains("alias, function or builtin")
        );
        assert_eq!(by_name("missing").status, AgentStatus::Missing);
        assert!(
            by_name("missing")
                .detail
                .contains("controlroom-no-such-agent")
        );
        assert_eq!(by_name("empty").status, AgentStatus::Missing);
    }
}

//! The agent launch command (spec §11.5, §6.1).
//!
//! Sandboxed sessions run
//! `nono run -s --no-rollback --profile <abs> --workdir <cwd> --allow-cwd -- $SHELL -lc '<cmd>'`
//! with controlroom-owned nono state and config dirs, no update check, and a dead registry
//! URL so an accidental registry fetch fails closed. Inherited `NONO_*` variables are removed:
//! nono reads flags such as `NONO_ALLOW` from the environment, which would widen the sandbox.

use std::ffi::OsString;
use std::process::Command;

use crate::paths::AbsPath;

/// Prefix of environment variables nono interprets as flags.
pub const NONO_ENV_PREFIX: &str = "NONO_";

/// A registry URL nothing listens on.
const DEAD_REGISTRY: &str = "http://127.0.0.1:9/";

/// Where nono keeps its own state (audit trail, session registry) and config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonoDirs {
    /// `XDG_STATE_HOME` for nono.
    pub state: AbsPath,
    /// `XDG_CONFIG_HOME` for nono.
    pub config: AbsPath,
}

impl NonoDirs {
    /// `<state_dir>/nono/state` and `<state_dir>/nono/config`.
    pub fn under(state_dir: &AbsPath) -> Self {
        Self {
            state: state_dir.join("nono/state"),
            config: state_dir.join("nono/config"),
        }
    }

    /// The environment nono itself runs with (also used for probes and validation).
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("XDG_STATE_HOME".to_owned(), self.state.to_string()),
            ("XDG_CONFIG_HOME".to_owned(), self.config.to_string()),
            ("NONO_NO_UPDATE_CHECK".to_owned(), "1".to_owned()),
            ("NONO_REGISTRY".to_owned(), DEAD_REGISTRY.to_owned()),
        ]
    }
}

/// How to run nono for a session.
#[derive(Debug, Clone)]
pub struct Nono {
    /// The pinned nono binary.
    pub binary: AbsPath,
    /// controlroom-owned nono dirs.
    pub dirs: NonoDirs,
}

/// A fully specified process launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    /// The program.
    pub program: String,
    /// Arguments after the program.
    pub args: Vec<String>,
    /// Variables to set.
    pub env: Vec<(String, String)>,
    /// Working directory.
    pub cwd: AbsPath,
}

impl LaunchCommand {
    /// Runs `command` (a shell command line) in a login shell, under nono when `nono` is
    /// given (levels strict and relaxed) or directly (level off).
    pub fn new(
        nono: Option<(&Nono, &AbsPath)>,
        shell: &AbsPath,
        cwd: &AbsPath,
        command: &str,
    ) -> Self {
        let shell_args = [shell.to_string(), "-lc".to_owned(), command.to_owned()];
        match nono {
            None => Self {
                program: shell.to_string(),
                args: shell_args[1..].to_vec(),
                env: Vec::new(),
                cwd: cwd.clone(),
            },
            Some((nono, profile)) => {
                let mut args: Vec<String> = [
                    "run",
                    "-s",
                    "--no-rollback",
                    "--profile",
                    profile.as_str(),
                    "--workdir",
                    cwd.as_str(),
                    "--allow-cwd",
                    "--",
                ]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
                args.extend(shell_args);
                Self {
                    program: nono.binary.to_string(),
                    args,
                    env: nono.dirs.env(),
                    cwd: cwd.clone(),
                }
            }
        }
    }

    /// A [`Command`] with the arguments, working directory and this process's environment
    /// minus every `NONO_*` variable, plus the launch variables.
    pub fn to_command(&self) -> Command {
        self.command_with_env(std::env::vars_os())
    }

    fn command_with_env(
        &self,
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .current_dir(self.cwd.as_path())
            .env_clear();
        cmd.envs(
            inherited
                .into_iter()
                .filter(|(k, _)| !k.to_string_lossy().starts_with(NONO_ENV_PREFIX)),
        );
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }
}

/// Quotes `s` for a POSIX shell (single quotes unless only safe characters).
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&b));
    if safe {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Joins argv into one shell command line.
pub fn shell_command(argv: &[&str]) -> String {
    argv.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> AbsPath {
        AbsPath::new(s).unwrap()
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("pi"), "pi");
        assert_eq!(shell_quote("--mode=rpc"), "--mode=rpc");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(
            shell_command(&["pi", "--mode", "rpc", "--session", "/a b/s.jsonl"]),
            "pi --mode rpc --session '/a b/s.jsonl'"
        );
    }

    #[test]
    fn sandboxed_argv_and_env() {
        let nono = Nono {
            binary: p("/nix/store/aaaa-nono-0.80.0/bin/nono"),
            dirs: NonoDirs::under(&p("/home/alice/.local/share/controlroom")),
        };
        let profile = p("/home/alice/.local/share/controlroom/sessions/S/profile.json");
        let l = LaunchCommand::new(
            Some((&nono, &profile)),
            &p("/run/current-system/sw/bin/bash"),
            &p("/home/alice/project"),
            "pi --mode rpc",
        );
        assert_eq!(l.program, "/nix/store/aaaa-nono-0.80.0/bin/nono");
        assert_eq!(
            l.args,
            [
                "run",
                "-s",
                "--no-rollback",
                "--profile",
                profile.as_str(),
                "--workdir",
                "/home/alice/project",
                "--allow-cwd",
                "--",
                "/run/current-system/sw/bin/bash",
                "-lc",
                "pi --mode rpc",
            ]
        );
        let env: std::collections::BTreeMap<_, _> = l.env.iter().cloned().collect();
        assert_eq!(
            env["XDG_STATE_HOME"],
            "/home/alice/.local/share/controlroom/nono/state"
        );
        assert_eq!(
            env["XDG_CONFIG_HOME"],
            "/home/alice/.local/share/controlroom/nono/config"
        );
        assert_eq!(env["NONO_NO_UPDATE_CHECK"], "1");
        assert_eq!(env["NONO_REGISTRY"], DEAD_REGISTRY);
    }

    #[test]
    fn unsandboxed_runs_the_shell_directly() {
        let l = LaunchCommand::new(None, &p("/bin/sh"), &p("/"), "exit 3");
        assert_eq!(l.program, "/bin/sh");
        assert_eq!(l.args, ["-lc", "exit 3"]);
        assert!(l.env.is_empty());
        let status = l.to_command().status().unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn to_command_strips_inherited_nono_vars() {
        let l = LaunchCommand {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "echo \"${NONO_ALLOW-unset}:$NONO_NO_UPDATE_CHECK\"".into(),
            ],
            env: vec![("NONO_NO_UPDATE_CHECK".into(), "1".into())],
            cwd: p("/"),
        };
        let inherited = [
            (OsString::from("NONO_ALLOW"), OsString::from("/")),
            (OsString::from("KEEP"), OsString::from("k")),
        ];
        let out = l.command_with_env(inherited).output().expect("run sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "unset:1");
    }
}

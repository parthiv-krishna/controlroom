//! Pure parsers for the files and command outputs `doctor` inspects.

/// Parses a systemd boolean (`yes`/`no`, `true`/`false`, `on`/`off`, `1`/`0`).
pub fn parse_systemd_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" | "y" | "t" => Some(true),
        "no" | "false" | "off" | "0" | "n" | "f" => Some(false),
        _ => None,
    }
}

/// The `[Login]` settings relevant to killing processes, merged across files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogindKillSettings {
    /// `KillUserProcesses`, and the file that set it last.
    pub kill_user_processes: Option<(bool, String)>,
    /// `KillOnlyUsers`; `None` if never set (an empty assignment resets it).
    pub kill_only_users: Option<Vec<String>>,
    /// `KillExcludeUsers`; `None` if never set (systemd's default is `root`).
    pub kill_exclude_users: Option<Vec<String>>,
}

/// Applies logind config files in precedence order (main file first, then drop-ins sorted by
/// name); later assignments win.
pub fn parse_logind_conf<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> LogindKillSettings {
    let mut out = LogindKillSettings::default();
    for (name, contents) in files {
        let mut in_login = false;
        for raw in contents.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                in_login = line == "[Login]";
                continue;
            }
            let Some((key, value)) = line.split_once('=').filter(|_| in_login) else {
                continue;
            };
            let value = value.trim();
            let users = || value.split_whitespace().map(str::to_owned).collect();
            match key.trim() {
                "KillUserProcesses" => {
                    if let Some(b) = parse_systemd_bool(value) {
                        out.kill_user_processes = Some((b, name.to_owned()));
                    }
                }
                "KillOnlyUsers" => out.kill_only_users = Some(users()),
                "KillExcludeUsers" => out.kill_exclude_users = Some(users()),
                _ => {}
            }
        }
    }
    out
}

/// Whether logind's user filters exempt `user` from `KillUserProcesses=yes` (logind.conf(5):
/// `KillExcludeUsers` defaults to `root`).
pub fn user_exempt_from_kill(settings: &LogindKillSettings, user: &str) -> bool {
    let excluded = match &settings.kill_exclude_users {
        Some(list) => list.iter().any(|u| u == user),
        None => user == "root",
    };
    let not_included = match &settings.kill_only_users {
        Some(list) if !list.is_empty() => !list.iter().any(|u| u == user),
        _ => false,
    };
    excluded || not_included
}

/// Parses `busctl get-property … KillUserProcesses` output (`b true`).
pub fn parse_busctl_bool(output: &str) -> Option<bool> {
    let mut parts = output.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("b"), Some(v)) => parse_systemd_bool(v),
        _ => None,
    }
}

/// Parses `loginctl show-user <user> -p Linger` output (`Linger=yes`).
pub fn parse_loginctl_linger(output: &str) -> Option<bool> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Linger="))
        .and_then(parse_systemd_bool)
}

/// Interprets `systemctl --user is-system-running`: whether the user manager answered (its
/// state is on stdout even with a non-zero exit, e.g. `degraded`), with the state or reason.
/// `None` when the output is unrecognized.
pub fn parse_user_manager_state(stdout: &str, stderr: &str) -> Option<(bool, String)> {
    let state = stdout.lines().next().unwrap_or_default().trim();
    match state {
        "running" | "degraded" | "starting" | "initializing" | "maintenance" | "stopping" => {
            Some((true, state.to_owned()))
        }
        "offline" | "unknown" => Some((false, state.to_owned())),
        _ if stderr.contains("Failed to connect") || stderr.contains("not defined") => Some((
            false,
            stderr.lines().next().unwrap_or_default().trim().to_owned(),
        )),
        _ => None,
    }
}

/// Where a process sits in the cgroup tree, from `/proc/self/cgroup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CgroupPlacement {
    /// Inside a logind session scope (`session-N.scope`): killed with the session when
    /// `KillUserProcesses=yes`, however it daemonizes.
    SessionScope {
        /// The cgroup path.
        path: String,
    },
    /// Under the user's systemd manager (`user@UID.service`): survives logout while the
    /// manager runs (i.e. with linger).
    UserManager {
        /// The cgroup path.
        path: String,
    },
    /// Anywhere else (a system service, a container root, no systemd).
    Other {
        /// The cgroup path.
        path: String,
    },
}

/// Parses `/proc/self/cgroup`, preferring the cgroup v1 `name=systemd` hierarchy (hybrid
/// setups) over the v2 (`0::`) line.
pub fn parse_proc_cgroup(contents: &str) -> Option<CgroupPlacement> {
    let mut v1 = None;
    let mut v2 = None;
    for line in contents.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if id == "0" && controllers.is_empty() {
            v2 = Some(path.trim());
        } else if controllers == "name=systemd" {
            v1 = Some(path.trim());
        }
    }
    let path = v1.or(v2)?;
    let has = |pred: fn(&str) -> bool| path.split('/').any(pred);
    let path = path.to_owned();
    Some(
        if has(|s| s.starts_with("user@") && s.ends_with(".service")) {
            CgroupPlacement::UserManager { path }
        } else if has(|s| s.starts_with("session-") && s.ends_with(".scope")) {
            CgroupPlacement::SessionScope { path }
        } else {
            CgroupPlacement::Other { path }
        },
    )
}

/// Interprets `crontab -l`: `Some(true)` when the user may use cron (a crontab exists, or "no
/// crontab for <user>"), `Some(false)` when denied.
pub fn parse_crontab_list(exit_ok: bool, stderr: &str) -> Option<bool> {
    if exit_ok {
        return Some(true);
    }
    let s = stderr.to_ascii_lowercase();
    if s.contains("no crontab for") {
        Some(true)
    } else if s.contains("not allowed") || s.contains("permission denied") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logind_conf_precedence_and_sections() {
        let main = "\
# See logind.conf(5)
[Login]
#KillUserProcesses=yes
KillUserProcesses=no
KillExcludeUsers=root
";
        let dropin = "\
[Login]
KillUserProcesses=yes
[Other]
KillUserProcesses=no
";
        let s = parse_logind_conf([("/etc/systemd/logind.conf", main)]);
        assert_eq!(
            s.kill_user_processes,
            Some((false, "/etc/systemd/logind.conf".into()))
        );
        let s = parse_logind_conf([
            ("/etc/systemd/logind.conf", main),
            ("/etc/systemd/logind.conf.d/10-kill.conf", dropin),
        ]);
        assert_eq!(
            s.kill_user_processes,
            Some((true, "/etc/systemd/logind.conf.d/10-kill.conf".into()))
        );
        assert_eq!(s.kill_exclude_users, Some(vec!["root".into()]));
        let commented_only = "[Login]\n#KillUserProcesses=no\n";
        assert_eq!(
            parse_logind_conf([("a", commented_only)]).kill_user_processes,
            None
        );
    }

    #[test]
    fn kill_user_filters() {
        let mut s = LogindKillSettings::default();
        assert!(user_exempt_from_kill(&s, "root"));
        assert!(!user_exempt_from_kill(&s, "alice"));
        s.kill_exclude_users = Some(vec!["alice".into()]);
        assert!(user_exempt_from_kill(&s, "alice"));
        s.kill_exclude_users = Some(vec![]);
        s.kill_only_users = Some(vec!["bob".into()]);
        assert!(user_exempt_from_kill(&s, "alice"));
        assert!(!user_exempt_from_kill(&s, "bob"));
    }

    #[test]
    fn busctl_and_loginctl() {
        assert_eq!(parse_busctl_bool("b false\n"), Some(false));
        assert_eq!(parse_busctl_bool("b true"), Some(true));
        assert_eq!(parse_busctl_bool("Failed to get property"), None);
        assert_eq!(parse_loginctl_linger("Linger=yes\n"), Some(true));
        assert_eq!(parse_loginctl_linger("Linger=no\n"), Some(false));
        assert_eq!(parse_loginctl_linger(""), None);
    }

    #[test]
    fn user_manager_state() {
        assert_eq!(
            parse_user_manager_state("running\n", ""),
            Some((true, "running".into()))
        );
        assert_eq!(
            parse_user_manager_state("degraded\n", ""),
            Some((true, "degraded".into()))
        );
        let stderr = "Failed to connect to user scope bus via local transport: $DBUS_SESSION_BUS_ADDRESS and $XDG_RUNTIME_DIR not defined\n";
        assert!(matches!(
            parse_user_manager_state("", stderr),
            Some((false, _))
        ));
        assert_eq!(
            parse_user_manager_state("offline\n", ""),
            Some((false, "offline".into()))
        );
        assert_eq!(parse_user_manager_state("", "weird"), None);
    }

    #[test]
    fn proc_cgroup() {
        assert_eq!(
            parse_proc_cgroup("0::/user.slice/user-1000.slice/session-4.scope\n"),
            Some(CgroupPlacement::SessionScope {
                path: "/user.slice/user-1000.slice/session-4.scope".into()
            })
        );
        assert!(matches!(
            parse_proc_cgroup(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/controlroomd.service\n"
            ),
            Some(CgroupPlacement::UserManager { .. })
        ));
        assert!(matches!(
            parse_proc_cgroup("0::/\n"),
            Some(CgroupPlacement::Other { .. })
        ));
        let hybrid = "\
12:memory:/user.slice
1:name=systemd:/user.slice/user-1000.slice/session-2.scope
0::/user.slice/user-1000.slice/session-2.scope
";
        assert!(matches!(
            parse_proc_cgroup(hybrid),
            Some(CgroupPlacement::SessionScope { .. })
        ));
        assert_eq!(parse_proc_cgroup(""), None);
    }

    #[test]
    fn crontab() {
        assert_eq!(parse_crontab_list(true, ""), Some(true));
        assert_eq!(
            parse_crontab_list(false, "no crontab for alice\n"),
            Some(true)
        );
        assert_eq!(
            parse_crontab_list(
                false,
                "You (alice) are not allowed to use this program (crontab)\n"
            ),
            Some(false)
        );
        assert_eq!(parse_crontab_list(false, "segfault"), None);
    }
}

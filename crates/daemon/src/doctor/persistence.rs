//! Persistence after logout and start at boot (prototype plan §6 step 4, spec §6.6).
//!
//! Every check degrades to `unknown` with the reason when a tool or file is missing; nothing
//! here fails the report.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use controlroom_protocol::host::{BootStartMethod, CheckStatus, DoctorCheck};

use super::parse::{self, CgroupPlacement};
use super::{check, unknown};
use crate::shell::{RunError, run};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// The invoking user.
pub struct Identity {
    /// The user name, when known.
    pub user: Option<String>,
    /// The uid.
    pub uid: u32,
}

/// The current user's name (from the environment or `id -un`) and uid.
pub fn identity() -> Identity {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            run("id", &["-un"], &[], COMMAND_TIMEOUT)
                .ok()
                .filter(|r| r.success)
                .map(|r| r.stdout.trim().to_owned())
        });
    Identity {
        user,
        uid: rustix::process::getuid().as_raw(),
    }
}

/// The persistence findings.
pub struct Persistence {
    /// Whether detached processes survive logout, when determinable.
    pub survives_logout: Option<bool>,
    /// `KillUserProcesses`, linger, the user manager and the cgroup placement.
    pub details: Vec<(&'static str, DoctorCheck)>,
    /// Linger is on.
    pub linger: Option<bool>,
    /// A systemd user manager answers.
    pub user_manager: Option<bool>,
}

impl Persistence {
    /// The summary check for the report.
    pub fn summary(&self) -> DoctorCheck {
        let cgroup = &self
            .details
            .last()
            .expect("the cgroup check is always present")
            .1;
        match self.survives_logout {
            Some(true) => check(CheckStatus::Ok, "yes", cgroup.detail.clone()),
            Some(false) => check(CheckStatus::Fail, "no", cgroup.detail.clone()),
            None => unknown(cgroup.detail.clone()),
        }
    }
}

/// Runs the persistence checks.
pub fn persistence(id: &Identity) -> Persistence {
    let (kill_check, kill) = kill_user_processes(id.user.as_deref());
    let (linger_check, linger) = linger(id.user.as_deref());
    let (manager_check, user_manager) = user_manager(id.uid);
    let (cgroup_check, killed) = cgroup(kill, linger);
    Persistence {
        survives_logout: killed.map(|k| !k),
        details: vec![
            ("KillUserProcesses", kill_check),
            ("linger", linger_check),
            ("systemd user manager", manager_check),
            ("cgroup", cgroup_check),
        ],
        linger,
        user_manager,
    }
}

/// logind.conf, then drop-ins, in systemd's precedence order: the first main file found in
/// /etc, /run, /usr/local/lib, /usr/lib; drop-ins from every `logind.conf.d`, a name in an
/// earlier dir masking later ones, applied in name order.
fn logind_config_files() -> Vec<(String, String)> {
    const DIRS: [&str; 4] = [
        "/etc/systemd",
        "/run/systemd",
        "/usr/local/lib/systemd",
        "/usr/lib/systemd",
    ];
    let mut files: Vec<(String, String)> = DIRS
        .iter()
        .find_map(|d| {
            let p = format!("{d}/logind.conf");
            std::fs::read_to_string(&p).ok().map(|c| (p, c))
        })
        .into_iter()
        .collect();
    let mut dropins: BTreeMap<String, String> = BTreeMap::new();
    for d in DIRS {
        let Ok(entries) = std::fs::read_dir(format!("{d}/logind.conf.d")) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".conf") {
                dropins
                    .entry(name)
                    .or_insert_with(|| e.path().to_string_lossy().into_owned());
            }
        }
    }
    files.extend(
        dropins
            .into_values()
            .filter_map(|p| std::fs::read_to_string(&p).ok().map(|c| (p, c))),
    );
    files
}

/// Effective `KillUserProcesses` for this user: `Some(true)` if the session scope is killed
/// at logout.
fn kill_user_processes(user: Option<&str>) -> (DoctorCheck, Option<bool>) {
    let files = logind_config_files();
    let settings = parse::parse_logind_conf(files.iter().map(|(p, c)| (p.as_str(), c.as_str())));
    let exempt = user.is_some_and(|u| parse::user_exempt_from_kill(&settings, u));
    // The running logind's value is authoritative; the config files are the fallback.
    let live = run(
        "busctl",
        &[
            "get-property",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "KillUserProcesses",
        ],
        &[],
        COMMAND_TIMEOUT,
    )
    .ok()
    .filter(|r| r.success)
    .and_then(|r| parse::parse_busctl_bool(&r.stdout));
    let (value, source) = match (live, &settings.kill_user_processes) {
        (Some(v), _) => (Some(v), "live value from logind".to_owned()),
        (None, Some((v, file))) => (Some(*v), file.clone()),
        (None, None) => (None, String::new()),
    };
    match value {
        Some(true) if exempt => (
            check(
                CheckStatus::Ok,
                "yes",
                format!("KillUserProcesses=yes ({source}), but this user is exempt"),
            ),
            Some(false),
        ),
        Some(true) => (
            check(
                CheckStatus::Warn,
                "yes",
                format!(
                    "KillUserProcesses=yes ({source}): processes left in the login session are killed at logout"
                ),
            ),
            Some(true),
        ),
        Some(false) => (
            check(
                CheckStatus::Ok,
                "no",
                format!("KillUserProcesses=no ({source})"),
            ),
            Some(false),
        ),
        None if files.is_empty() => (
            unknown("no logind configuration found and logind not reachable (no systemd?)"),
            None,
        ),
        None => (
            unknown("KillUserProcesses is not set; the compiled-in default applies (usually no)"),
            None,
        ),
    }
}

fn linger(user: Option<&str>) -> (DoctorCheck, Option<bool>) {
    let Some(user) = user else {
        return (unknown("cannot determine the user name"), None);
    };
    let on = || {
        (
            check(CheckStatus::Ok, "yes", "linger is enabled"),
            Some(true),
        )
    };
    let off = || {
        (
            check(
                CheckStatus::Warn,
                "no",
                format!(
                    "linger is off; enable it with `loginctl enable-linger {user}` (may need admin rights)"
                ),
            ),
            Some(false),
        )
    };
    let result = run(
        "loginctl",
        &["show-user", user, "-p", "Linger"],
        &[],
        COMMAND_TIMEOUT,
    );
    if let Ok(r) = &result
        && r.success
    {
        return match parse::parse_loginctl_linger(&r.stdout) {
            Some(true) => on(),
            Some(false) => off(),
            None => (
                unknown(format!("unexpected loginctl output: {}", r.stdout.trim())),
                None,
            ),
        };
    }
    // loginctl fails for users without a session; the linger flag file is authoritative.
    let dir = Path::new("/var/lib/systemd/linger");
    if dir.join(user).exists() {
        on()
    } else if dir.is_dir() {
        off()
    } else {
        let why = match result {
            Ok(r) => r.stderr.trim().to_owned(),
            Err(e) => e.describe("loginctl"),
        };
        (unknown(why), None)
    }
}

fn user_manager(uid: u32) -> (DoctorCheck, Option<bool>) {
    // Over SSH, pam_systemd normally sets XDG_RUNTIME_DIR; fill it in if not. (Only to reach
    // the user bus; controlroom never stores anything there.)
    let runtime_dir = format!("/run/user/{uid}");
    let mut env = Vec::new();
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        env.push(("XDG_RUNTIME_DIR", runtime_dir.as_str()));
    }
    match run(
        "systemctl",
        &["--user", "is-system-running"],
        &env,
        COMMAND_TIMEOUT,
    ) {
        Ok(r) => match parse::parse_user_manager_state(&r.stdout, &r.stderr) {
            Some((true, state)) => (
                check(
                    CheckStatus::Ok,
                    "yes",
                    format!("systemd user manager is {state}"),
                ),
                Some(true),
            ),
            Some((false, why)) => (
                check(
                    CheckStatus::Warn,
                    "no",
                    format!("systemd user manager not reachable: {why}"),
                ),
                Some(false),
            ),
            None => (
                unknown(format!(
                    "unexpected `systemctl --user` output: {} {}",
                    r.stdout.trim(),
                    r.stderr.trim()
                )),
                None,
            ),
        },
        Err(RunError::NotFound) => (
            check(
                CheckStatus::Warn,
                "no",
                "`systemctl` not found (no systemd)",
            ),
            Some(false),
        ),
        Err(e) => (unknown(e.describe("systemctl")), None),
    }
}

/// Returns the check and whether detached processes would be killed at logout.
fn cgroup(kill: Option<bool>, linger: Option<bool>) -> (DoctorCheck, Option<bool>) {
    let contents = match std::fs::read_to_string("/proc/self/cgroup") {
        Ok(c) => c,
        Err(e) => return (unknown(format!("cannot read /proc/self/cgroup: {e}")), None),
    };
    let Some(placement) = parse::parse_proc_cgroup(&contents) else {
        return (unknown("unrecognized /proc/self/cgroup"), None);
    };
    match placement {
        CgroupPlacement::SessionScope { path } => match kill {
            Some(true) => (
                check(
                    CheckStatus::Fail,
                    "session_scope",
                    format!(
                        "processes started here stay in {path} even after setsid, and KillUserProcesses=yes kills that scope at logout"
                    ),
                ),
                Some(true),
            ),
            Some(false) => (
                check(
                    CheckStatus::Ok,
                    "session_scope",
                    format!(
                        "in {path}; detached processes survive logout because KillUserProcesses=no"
                    ),
                ),
                Some(false),
            ),
            None => (
                check(
                    CheckStatus::Unknown,
                    "session_scope",
                    format!("in {path}; survival depends on KillUserProcesses, which is unknown"),
                ),
                None,
            ),
        },
        CgroupPlacement::UserManager { path } => match linger {
            Some(true) => (
                check(
                    CheckStatus::Ok,
                    "user_manager",
                    format!("in {path}; with linger the user manager outlives logout"),
                ),
                Some(false),
            ),
            Some(false) => (
                check(
                    CheckStatus::Warn,
                    "user_manager",
                    format!(
                        "in {path}; without linger the user manager stops at the last logout, taking its processes with it"
                    ),
                ),
                Some(true),
            ),
            None => (
                check(
                    CheckStatus::Unknown,
                    "user_manager",
                    format!("in {path}; linger unknown"),
                ),
                None,
            ),
        },
        CgroupPlacement::Other { path } => (
            check(
                CheckStatus::Ok,
                "other",
                format!("in {path}; not managed by a logind session"),
            ),
            Some(false),
        ),
    }
}

/// How the daemon can start after a reboot.
pub struct BootStart {
    /// The best available method.
    pub method: BootStartMethod,
    /// Details of the chosen method.
    pub summary: DoctorCheck,
    /// The systemd user unit and crontab checks.
    pub details: Vec<(&'static str, DoctorCheck)>,
}

/// Linger plus a systemd user unit, else `crontab @reboot`, else lazily on the next connect.
pub fn boot_start(p: &Persistence) -> BootStart {
    let systemd_user = match (p.linger, p.user_manager) {
        (Some(true), Some(true)) => check(
            CheckStatus::Ok,
            "yes",
            "linger is on and the user manager answers: a systemd user unit starts at boot",
        ),
        (Some(false), _) => check(
            CheckStatus::Warn,
            "no",
            "needs linger, so that the user manager starts at boot",
        ),
        (_, Some(false)) => check(CheckStatus::Warn, "no", "no reachable systemd user manager"),
        _ => unknown("linger or user manager state unknown"),
    };
    let cron = match run("crontab", &["-l"], &[], COMMAND_TIMEOUT) {
        Ok(r) => match parse::parse_crontab_list(r.success, &r.stderr) {
            Some(true) => check(CheckStatus::Ok, "yes", "`crontab` is available for @reboot"),
            Some(false) => check(
                CheckStatus::Warn,
                "no",
                format!("cron denied: {}", r.stderr.trim()),
            ),
            None => unknown(format!(
                "unexpected `crontab -l` result: {}",
                r.stderr.trim()
            )),
        },
        Err(RunError::NotFound) => check(CheckStatus::Warn, "no", "`crontab` not found"),
        Err(e) => unknown(e.describe("crontab")),
    };
    let (method, summary) = if systemd_user.status == CheckStatus::Ok {
        (BootStartMethod::SystemdUser, systemd_user.clone())
    } else if cron.status == CheckStatus::Ok {
        (BootStartMethod::Cron, cron.clone())
    } else {
        (
            BootStartMethod::Lazy,
            check(
                CheckStatus::Warn,
                "lazy",
                "the daemon starts on the next connection; enable linger (systemd) or allow cron to start it at boot",
            ),
        )
    };
    BootStart {
        method,
        summary,
        details: vec![("systemd user unit", systemd_user), ("crontab", cron)],
    }
}

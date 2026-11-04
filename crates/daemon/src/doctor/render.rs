//! The human-readable doctor table.

use std::fmt::Write;

use controlroom_protocol::host::{BootStartMethod, CheckStatus, DoctorCheck};

use super::Diagnosis;

fn mark(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Ok => "ok  ",
        CheckStatus::Warn => "warn",
        CheckStatus::Fail => "FAIL",
        CheckStatus::Unknown => "??  ",
    }
}

fn row(out: &mut String, label: &str, check: &DoctorCheck) {
    let value = check
        .value
        .as_deref()
        .map(|v| format!("{v}: "))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "  [{}] {label:<22} {value}{}",
        mark(check.status),
        check.detail
    );
}

/// Renders a diagnosis as a table, one line per check.
pub fn render_human(d: &Diagnosis) -> String {
    let r = &d.report;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "controlroomd {} doctor (user: {})",
        r.daemon_version,
        r.user.as_deref().unwrap_or("unknown")
    );

    let survives = match (r.persistence.status, r.persistence.value.as_deref()) {
        (CheckStatus::Ok, _) => "yes",
        (CheckStatus::Fail, _) => "NO",
        (_, value) => value.unwrap_or("unknown"),
    };
    let _ = writeln!(out, "\nPersistence after logout: {survives}");
    for (label, check) in &d.persistence {
        row(&mut out, label, check);
    }

    let method = match r.boot_start {
        BootStartMethod::SystemdUser => "systemd user unit (linger)",
        BootStartMethod::Cron => "crontab @reboot",
        BootStartMethod::Lazy => "lazy (on the next connection)",
        BootStartMethod::Unknown => "unknown",
    };
    let _ = writeln!(out, "\nStart at boot: {method}");
    for (label, check) in &d.boot_start {
        row(&mut out, label, check);
    }

    let _ = writeln!(out, "\nSandbox:");
    if let Some(e) = &d.sandbox_config_error {
        let _ = writeln!(out, "  [warn] {e}");
    }
    row(&mut out, "nono", &r.nono);
    row(&mut out, "Landlock ABI", &d.landlock);
    row(&mut out, "sftp", &d.sftp);
    out
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::host::DoctorReport;

    use super::super::{check, unknown};
    use super::*;

    fn sample() -> Diagnosis {
        Diagnosis {
            report: DoctorReport {
                daemon_version: "0.1.0".into(),
                user: Some("alice".into()),
                persistence: check(CheckStatus::Fail, "no", "killed at logout"),
                boot_start: BootStartMethod::Lazy,
                boot_start_check: check(CheckStatus::Warn, "lazy", "on the next connection"),
                nono: check(
                    CheckStatus::Ok,
                    "0.80.0",
                    "/nix/store/aaaa-nono-0.80.0/bin/nono",
                ),
                landlock_abi: Some(6),
                agents_config: None,
                agents_config_error: None,
                agents: Vec::new(),
                checked_at: UnixMillis(1_000),
            },
            persistence: vec![
                (
                    "KillUserProcesses",
                    check(CheckStatus::Warn, "yes", "KillUserProcesses=yes"),
                ),
                ("linger", check(CheckStatus::Warn, "no", "linger is off")),
                ("systemd user manager", unknown("`systemctl` timed out")),
                (
                    "cgroup",
                    check(CheckStatus::Fail, "session_scope", "killed at logout"),
                ),
            ],
            boot_start: vec![
                (
                    "systemd user unit",
                    check(CheckStatus::Warn, "no", "needs linger"),
                ),
                (
                    "crontab",
                    check(CheckStatus::Warn, "no", "`crontab` not found"),
                ),
            ],
            landlock: check(CheckStatus::Ok, "6", "filesystem and network confinement"),
            sftp: unknown("no sftp-server"),
            sandbox_config_error: Some("sandbox.toml: bad; using defaults".into()),
        }
    }

    #[test]
    fn table() {
        let expected = "\
controlroomd 0.1.0 doctor (user: alice)

Persistence after logout: NO
  [warn] KillUserProcesses      yes: KillUserProcesses=yes
  [warn] linger                 no: linger is off
  [??  ] systemd user manager   `systemctl` timed out
  [FAIL] cgroup                 session_scope: killed at logout

Start at boot: lazy (on the next connection)
  [warn] systemd user unit      no: needs linger
  [warn] crontab                no: `crontab` not found

Sandbox:
  [warn] sandbox.toml: bad; using defaults
  [ok  ] nono                   0.80.0: /nix/store/aaaa-nono-0.80.0/bin/nono
  [ok  ] Landlock ABI           6: filesystem and network confinement
  [??  ] sftp                   no sftp-server
";
        assert_eq!(render_human(&sample()), expected);
    }

    #[test]
    fn json_is_the_protocol_report() {
        let v = serde_json::to_value(&sample().report).unwrap();
        assert_eq!(v["persistence"]["status"], "fail");
        assert_eq!(v["boot_start"], "lazy");
        assert_eq!(v["nono"]["value"], "0.80.0");
        assert_eq!(v["landlock_abi"], 6);
        assert_eq!(v["agents"], serde_json::json!([]));
        assert_eq!(v["checked_at"], 1000);
    }
}

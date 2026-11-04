//! The `controlroomd` binary: `--version` and `doctor`.

use std::process::{Command, Stdio};

use controlroom_protocol::host::DoctorReport;

const BIN: &str = env!("CARGO_BIN_EXE_controlroomd");

#[test]
fn version_first_line() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().next(),
        Some(format!("controlroomd {}", env!("CARGO_PKG_VERSION")).as_str())
    );
}

#[test]
fn doctor_prints_the_protocol_report_and_a_table() {
    let home = tempfile::tempdir().unwrap();
    let doctor = |args: &[&str]| {
        let out = Command::new(BIN)
            .arg("doctor")
            .args(args)
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let report: DoctorReport = serde_json::from_str(&doctor(&[])).unwrap();
    assert_eq!(report.daemon_version, env!("CARGO_PKG_VERSION"));
    assert!(report.agents.is_empty(), "agents run on the server");

    let config = home.path().join(".config/controlroom");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("sandbox.toml"),
        "nono = \"/nonexistent/nono\"\n",
    )
    .unwrap();
    let table = doctor(&["--human"]);
    for heading in ["Persistence after logout:", "Start at boot:", "Sandbox:"] {
        assert!(table.contains(heading), "{table}");
    }
    assert!(table.contains("[FAIL] nono"), "{table}");
}

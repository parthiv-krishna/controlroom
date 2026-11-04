//! Drift check for the generated TypeScript: `web/src/lib/protocol/` (UI API) and
//! `extensions/pi/src/protocol/` (agent API).
//!
//! - `cargo test -p controlroom-protocol --test typescript` regenerates into memory and fails
//!   when the checked-in files differ.
//! - `CONTROLROOM_UPDATE_TS=1 cargo test -p controlroom-protocol --test typescript` rewrites the
//!   checked-in files instead.
//! - With `CONTROLROOM_TS_DRIFT_REQUIRED` set (the `protocol-ts-drift` Nix check), a source tree
//!   without the checked-in files is an error instead of a skip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use controlroom_protocol::typescript::{self, AGENT_DIR, REGENERATE_COMMAND, WEB_DIR};

fn checked_in_dir(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn check_drift(relative: &str, expected: BTreeMap<PathBuf, String>) {
    let checked_in = checked_in_dir(relative);

    if std::env::var_os("CONTROLROOM_UPDATE_TS").is_some() {
        typescript::write_to(&checked_in, expected).expect("write TypeScript bindings");
        eprintln!("regenerated {}", checked_in.display());
        return;
    }

    let actual = if checked_in.exists() {
        typescript::list_ts_files(&checked_in).expect("list checked-in files")
    } else {
        Vec::new()
    };
    if actual.is_empty() {
        assert!(
            std::env::var_os("CONTROLROOM_TS_DRIFT_REQUIRED").is_none(),
            "CONTROLROOM_TS_DRIFT_REQUIRED is set but {} has no .ts files",
            checked_in.display()
        );
        // Builds that only see the Cargo sources (the plain `test` check) have nothing to
        // compare against; the `protocol-ts-drift` check covers them.
        eprintln!(
            "skipping TypeScript drift check: no .ts files in {}",
            checked_in.display()
        );
        return;
    }

    let mut problems = Vec::new();
    for (path, want) in &expected {
        if !actual.contains(path) {
            problems.push(format!("missing: {}", path.display()));
        } else if std::fs::read_to_string(checked_in.join(path)).expect("read checked-in file")
            != *want
        {
            problems.push(format!("outdated: {}", path.display()));
        }
    }
    for path in &actual {
        if !expected.contains_key(path) {
            problems.push(format!("stale (no longer generated): {}", path.display()));
        }
    }

    assert!(
        problems.is_empty(),
        "{relative}/ is out of date with crates/protocol:\n  {}\n\nRun `{}` to regenerate.",
        problems.join("\n  "),
        REGENERATE_COMMAND
    );
}

#[test]
fn typescript_bindings_are_up_to_date() {
    check_drift(WEB_DIR, typescript::generate().expect("generate"));
}

#[test]
fn agent_typescript_bindings_are_up_to_date() {
    check_drift(AGENT_DIR, typescript::generate_agent().expect("generate"));
}

#[test]
fn generation_is_deterministic() {
    assert_eq!(
        typescript::generate().unwrap(),
        typescript::generate().unwrap()
    );
    assert_eq!(
        typescript::generate_agent().unwrap(),
        typescript::generate_agent().unwrap()
    );
}

fn assert_generated(files: &BTreeMap<PathBuf, String>, names: &[&str]) {
    for name in names {
        assert!(files.contains_key(Path::new(name)), "{name} not generated");
    }
}

#[test]
fn generated_files_cover_the_api() {
    let files = typescript::generate().unwrap();
    assert_generated(
        &files,
        &[
            "index.ts",
            "UiMethods.ts",
            "versions.ts",
            "SessionSummary.ts",
            "FeedSession.ts",
            "Workspace.ts",
            "Shell.ts",
            "ShellState.ts",
            "Availability.ts",
            "Overlap.ts",
            "Project.ts",
            "Task.ts",
            "Schedule.ts",
            "SessionDetail.ts",
            "TimelineItem.ts",
            "TimelineOp.ts",
            "Request.ts",
            "Machine.ts",
            "Connectivity.ts",
            "ErrorData.ts",
            "RpcError.ts",
            "RpcRequest.ts",
            "RpcResponse.ts",
            "RpcNotification.ts",
            "RemotePath.ts",
        ],
    );
    // The UI API alone: no daemon map, no per-machine session wrapper or location prefixes.
    for name in [
        "DaemonMethods.ts",
        "MachineSession.ts",
        "SessionLink.ts",
        "LocationPrefix.ts",
    ] {
        assert!(!files.contains_key(Path::new(name)), "{name} in the UI set");
    }
    let agent = typescript::generate_agent().unwrap();
    assert_generated(
        &agent,
        &[
            "index.ts",
            "AgentMethods.ts",
            "versions.ts",
            "AgentState.ts",
            "QuestionAskParams.ts",
            "Todo.ts",
            "SessionInfo.ts",
            "HostCallParams.ts",
            "HostOp.ts",
            "HostResult.ts",
        ],
    );
    assert!(!agent.contains_key(Path::new("DaemonMethods.ts")));
    // Ids, cursors and timestamps are plain TS types, never bigint or JsonValue; doc comments
    // never close early (a `*/` inside a Rust doc, e.g. a `**/*` glob, would end the comment).
    for (path, source) in files.iter().chain(&agent) {
        assert!(!source.contains("bigint"), "{} uses bigint", path.display());
        assert_eq!(
            source.matches("/**").count(),
            source.matches("*/").count(),
            "{} has a doc comment that closes early",
            path.display()
        );
        assert!(
            !source.contains("JsonValue"),
            "{} uses JsonValue",
            path.display()
        );
    }
    assert_eq!(
        files[Path::new("SessionId.ts")].lines().last(),
        Some("export type SessionId = string;")
    );
    assert_eq!(
        files[Path::new("Seq.ts")].lines().last(),
        Some("export type Seq = number;")
    );
}

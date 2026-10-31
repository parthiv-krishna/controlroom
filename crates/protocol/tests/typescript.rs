//! Drift check for the generated TypeScript in `web/src/lib/protocol/`.
//!
//! - `cargo test -p controlroom-protocol --test typescript` regenerates into a temp dir and fails
//!   when the checked-in files differ.
//! - `CONTROLROOM_UPDATE_TS=1 cargo test -p controlroom-protocol --test typescript` rewrites the
//!   checked-in files instead.
//! - With `CONTROLROOM_TS_DRIFT_REQUIRED` set (the `protocol-ts-drift` Nix check), a source tree
//!   without the checked-in files is an error instead of a skip.

use std::path::{Path, PathBuf};

use controlroom_protocol::typescript::{self, REGENERATE_COMMAND};

fn checked_in_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/lib/protocol")
}

#[test]
fn typescript_bindings_are_up_to_date() {
    let checked_in = checked_in_dir();

    if std::env::var_os("CONTROLROOM_UPDATE_TS").is_some() {
        typescript::write_to(&checked_in).expect("write TypeScript bindings");
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

    let expected = typescript::generate().expect("generate TypeScript bindings");
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
        "web/src/lib/protocol/ is out of date with crates/protocol:\n  {}\n\nRun `{}` to regenerate.",
        problems.join("\n  "),
        REGENERATE_COMMAND
    );
}

#[test]
fn generation_is_deterministic() {
    assert_eq!(
        typescript::generate().unwrap(),
        typescript::generate().unwrap()
    );
}

#[test]
fn generated_files_cover_the_api() {
    let files = typescript::generate().unwrap();
    for name in [
        "index.ts",
        "DaemonMethods.ts",
        "UiMethods.ts",
        "versions.ts",
        "SessionSummary.ts",
        "SessionDetail.ts",
        "TimelineItem.ts",
        "TimelineOp.ts",
        "Request.ts",
        "MachineSession.ts",
        "Machine.ts",
        "Connectivity.ts",
        "Task.ts",
        "Schedule.ts",
        "ErrorData.ts",
        "RpcError.ts",
        "RpcRequest.ts",
        "RpcResponse.ts",
        "RpcNotification.ts",
        "RemotePath.ts",
    ] {
        assert!(files.contains_key(Path::new(name)), "{name} not generated");
    }
    // Ids, cursors and timestamps are plain TS types, never bigint or JsonValue.
    for (path, source) in &files {
        assert!(!source.contains("bigint"), "{} uses bigint", path.display());
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

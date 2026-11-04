//! File and search ops through the fs worker: windowed reads, CAS writes, ls, find, grep on a
//! large file.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use controlroom_protocol::host::{ContentEncoding, HostOp, HostResult, ReadContent};
use controlroom_protocol::session::SandboxLevel;
use controlroom_protocol::{ErrorData, RemotePath};

use crate::harness::{HostEnv, Session};

fn read(path: &str, offset: Option<u64>, limit: Option<u64>) -> HostOp {
    HostOp::Read {
        path: path.into(),
        offset,
        limit,
    }
}

#[tokio::test]
async fn reads_and_cas_writes() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let project = env.project();

    // Write a new file (parents created), relative to the root.
    let written = session
        .run(
            &client,
            "w1",
            HostOp::Write {
                path: "src/main.rs".into(),
                content: "fn main() {}\n".into(),
                encoding: ContentEncoding::Utf8,
                expect_sha256: None,
            },
        )
        .await
        .unwrap();
    let HostResult::Write { path, bytes, .. } = written else {
        panic!()
    };
    assert_eq!(path.as_str(), project.join("src/main.rs").to_str().unwrap());
    assert_eq!(bytes, 13);

    // An edit: read the bytes, then write with the hash.
    let HostResult::ReadBytes { data, sha256, .. } = session
        .run(
            &client,
            "e1/read",
            HostOp::ReadBytes {
                path: "~/project/src/main.rs".into(),
                offset: None,
                length: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(STANDARD.decode(data).unwrap(), b"fn main() {}\n");
    let edit = |call: &str, expect: Option<String>| {
        session.run_params(
            call,
            HostOp::Write {
                path: "src/main.rs".into(),
                content: STANDARD.encode("fn main() { println!(); }\n"),
                encoding: ContentEncoding::Base64,
                expect_sha256: expect,
            },
        )
    };
    client
        .call::<controlroom_protocol::host::methods::ToolRun>(edit("e1/write", sha256.clone()))
        .await
        .unwrap();
    // A stale hash is a conflict, and nothing is written.
    std::fs::write(project.join("src/main.rs"), "changed by someone\n").unwrap();
    let err = client
        .call::<controlroom_protocol::host::methods::ToolRun>(edit("e2/write", sha256))
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::Conflict { .. })),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("src/main.rs")).unwrap(),
        "changed by someone\n"
    );

    // stat, ls.
    let HostResult::Stat { exists, size, .. } = session
        .run(
            &client,
            "s1",
            HostOp::Stat {
                path: "src/main.rs".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!((exists, size), (true, Some(19)));
    let HostResult::Ls { entries, total, .. } = session
        .run(
            &client,
            "l1",
            HostOp::Ls {
                path: None,
                limit: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!((names, total), (vec!["src", "sub"], 2));

    // A missing file is an fs error with the resolved path.
    let err = session
        .run(&client, "r0", read("nope.txt", None, None))
        .await
        .unwrap_err();
    assert!(
        matches!(&err.data, Some(ErrorData::FsError { path, .. }) if path == &RemotePath::from(project.join("nope.txt").to_str().unwrap())),
        "{err:?}"
    );
}

#[tokio::test]
async fn large_files_windowed_reads_and_grep() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let big = env.project().join("big.log");
    let text: String = (1..=300_000)
        .map(|i| {
            if i % 50_000 == 0 {
                format!("line {i} NEEDLE\n")
            } else {
                format!("line {i} filler filler filler\n")
            }
        })
        .collect();
    std::fs::write(&big, text).unwrap();
    std::fs::write(env.project().join("sub/notes.md"), "a NEEDLE in notes\n").unwrap();
    std::fs::write(env.project().join(".gitignore"), "ignored/\n").unwrap();
    std::fs::create_dir_all(env.project().join(".git")).unwrap();
    std::fs::create_dir_all(env.project().join("ignored")).unwrap();
    std::fs::write(env.project().join("ignored/x.md"), "NEEDLE\n").unwrap();

    let HostResult::Read { content, .. } = session
        .run(&client, "r1", read("big.log", Some(299_999), None))
        .await
        .unwrap()
    else {
        panic!()
    };
    let ReadContent::Text {
        lines,
        start_line,
        total_lines,
        ..
    } = content
    else {
        panic!()
    };
    assert_eq!((start_line, total_lines), (299_999, 300_001));
    assert_eq!(lines[0], "line 299999 filler filler filler");

    let HostResult::Grep {
        lines,
        matches,
        limit_reached,
        ..
    } = session
        .run(
            &client,
            "g1",
            HostOp::Grep {
                pattern: "needle".into(),
                path: None,
                glob: None,
                ignore_case: true,
                literal: true,
                context: None,
                limit: Some(4),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!((matches, limit_reached), (4, true));
    assert!(lines.iter().all(|l| l.path == "big.log"), "{lines:?}");
    assert_eq!(lines[0].line, 50_000);

    // Ignore files apply (in a git repository), and globs filter.
    let HostResult::Grep { lines, .. } = session
        .run(
            &client,
            "g2",
            HostOp::Grep {
                pattern: "NEEDLE".into(),
                path: None,
                glob: Some("*.md".into()),
                ignore_case: false,
                literal: false,
                context: None,
                limit: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let paths: Vec<_> = lines.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(paths, ["sub/notes.md"]);

    let HostResult::Find { paths, .. } = session
        .run(
            &client,
            "f1",
            HostOp::Find {
                pattern: "*.md".into(),
                path: None,
                limit: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(paths, ["sub/notes.md"]);
}

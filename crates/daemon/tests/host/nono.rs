//! Workspaces under real nono (spec §11, host side). Ignored by default: they need nono 0.80
//! and Landlock. Run on a dev machine with
//!
//! ```text
//! nix build .#nono -o /tmp/cr-nono
//! CR_TEST_NONO=/tmp/cr-nono/bin/nono cargo test -p controlroomd --test host nono -- --ignored
//! ```
//!
//! `nix build .#nono-x86_64-linux` (the static build bundled for the hosts) works as well.

use controlroom_protocol::ErrorData;
use std::sync::Arc;

use controlroom_protocol::host::{
    ApprovalRespondParams, HostApproval, HostOp, HostResult, JobsSubscribeParams, methods,
};
use controlroom_protocol::jsonrpc::NoParams;
use controlroom_protocol::session::SandboxLevel;

use crate::harness::{HostEnv, RpcClient, Session};

/// Denies every sandbox approval (nono asks before reaching outside the profile); returns how
/// many it denied so far.
async fn deny_everything(env: &HostEnv) -> Arc<std::sync::atomic::AtomicUsize> {
    answer_everything(env, "deny").await
}

/// Answers every sandbox approval with `option_id`; returns how many it answered so far.
async fn answer_everything(env: &HostEnv, option_id: &str) -> Arc<std::sync::atomic::AtomicUsize> {
    let approver = env.controller().await;
    approver
        .call::<methods::ApprovalsSubscribe>(NoParams {})
        .await
        .unwrap();
    let answered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = answered.clone();
    let option_id = option_id.to_owned();
    tokio::spawn(async move {
        let approver: RpcClient = approver;
        loop {
            let Some((method, note)) = approver.notes.lock().await.recv().await else {
                return;
            };
            if method != "approval.request" {
                continue;
            }
            let approval: HostApproval = serde_json::from_value(note["approval"].clone()).unwrap();
            let _ = approver
                .call::<methods::ApprovalRespond>(ApprovalRespondParams {
                    approval_id: approval.id,
                    option_id: option_id.clone(),
                    reason: Some("outside the workspace".into()),
                })
                .await;
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    answered
}

/// A scratch home under `/var/tmp` whose `sandbox.toml` points at the test's nono.
async fn env() -> HostEnv {
    let nono = std::env::var("CR_TEST_NONO").unwrap_or_else(|_| "nono".into());
    env_with(&format!("nono = {nono:?}\n"), |_| {}).await
}

/// A scratch home under `/var/tmp` with `sandbox_toml` (plus `nono_tmp`), set up by `prepare`.
async fn env_with(sandbox_toml: &str, prepare: impl FnOnce(&mut HostEnv)) -> HostEnv {
    let mut env = HostEnv::new_in("/var/tmp");
    prepare(&mut env);
    let config = env.home().join(".config/controlroom");
    std::fs::create_dir_all(&config).unwrap();
    // nono's own temp dirs go into the scratch dir, so the harness can clean what a killed
    // nono leaves (a short path: nono binds sockets in it).
    let nono_tmp = env.dir.path().join("t");
    std::fs::create_dir(&nono_tmp).unwrap();
    std::fs::write(
        config.join("sandbox.toml"),
        format!("{sandbox_toml}nono_tmp = {nono_tmp:?}\n"),
    )
    .unwrap();
    std::fs::write(env.home().join("secret.txt"), "outside the workspace\n").unwrap();
    // The stand-in login shell lives outside every sandbox; jobs need a real one.
    env.login_shell = crate::harness::bash();
    env.start_daemon().await;
    env
}

/// The nono the controller installs next to the daemon (no `nono` in `sandbox.toml`) confines
/// shells: CR_TEST_NONO stands in for the bundled static build.
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn the_bundled_nono_confines_shells() {
    let nono = std::path::PathBuf::from(std::env::var("CR_TEST_NONO").unwrap());
    let mut installed = None;
    let env = env_with("", |env| installed = Some(env.install(&nono, "0.80.0"))).await;
    let installed = std::fs::canonicalize(installed.unwrap()).unwrap();
    let denied = deny_everything(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    session.shell(&client, "s").await;
    let (exit, output, _) = session.bash(&client, "c1", "s", "echo ok").await;
    assert_eq!((exit, output.as_str()), (Some(0), "ok"), "{}", env.log());
    let (exit, _, _) = session.bash(&client, "c2", "s", "cat ~/secret.txt").await;
    assert_ne!(exit, Some(0), "the shell read outside the workspace");
    assert!(denied.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    // The shell runs under the installed nono (by its command line: nono makes itself
    // non-dumpable, so its `/proc/<pid>/exe` is unreadable).
    let run = format!("{}\0run\0", installed.display());
    let under_bundled = std::fs::read_dir("/proc").unwrap().flatten().any(|p| {
        std::fs::read(p.path().join("cmdline")).is_ok_and(|c| c.starts_with(run.as_bytes()))
    });
    assert!(under_bundled, "no process runs {}", installed.display());
}

/// An approved `git push` (a mediated command, which nono runs in its own sandbox) works under
/// the bundled nono, installed in the daemon's data dir.
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn an_approved_git_push_runs_under_the_bundled_nono() {
    let nono = std::path::PathBuf::from(std::env::var("CR_TEST_NONO").unwrap());
    let env = env_with("", |env| {
        env.install(&nono, "0.80.0");
    })
    .await;
    let git = |dir: &std::path::Path, args: &str| {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("git {args}"))
            .current_dir(dir)
            .env("HOME", env.home())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args}: {out:?}");
    };
    // A repository and its remote in the workspace.
    let push = env.project();
    std::fs::create_dir_all(push.join("repo")).unwrap();
    git(&push, "init -q --bare remote.git");
    git(&push.join("repo"), "init -q");
    git(
        &push.join("repo"),
        "-c user.email=alice@example.com -c user.name=alice commit -q --allow-empty -m init",
    );
    git(&push.join("repo"), "remote add origin ../remote.git");
    let allowed = answer_everything(&env, "allow-once").await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    session.shell(&client, "s").await;
    let (exit, output, _) = session
        .bash(
            &client,
            "p1",
            "s",
            "cd repo && git push origin HEAD:allowed 2>&1",
        )
        .await;
    assert_eq!(exit, Some(0), "{output}\n{}", env.log());
    assert!(allowed.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    git(
        &push.join("remote.git"),
        "rev-parse -q --verify refs/heads/allowed",
    );
}

#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn the_fs_worker_is_confined_to_the_workspace() {
    let env = env().await;
    let denied = deny_everything(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    std::fs::write(env.project().join("inside.txt"), "inside\n").unwrap();
    let inside = session
        .run(
            &client,
            "r1",
            HostOp::Read {
                path: "inside.txt".into(),
                offset: None,
                limit: None,
            },
        )
        .await;
    assert!(
        matches!(inside, Ok(HostResult::Read { .. })),
        "{inside:?}\n{}",
        env.log()
    );
    let outside = session
        .run(
            &client,
            "r2",
            HostOp::Read {
                path: "~/secret.txt".into(),
                offset: None,
                limit: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(outside.data, Some(ErrorData::FsError { .. })),
        "{outside:?}"
    );
    let write = session
        .run(
            &client,
            "w1",
            HostOp::Write {
                path: "~/planted.txt".into(),
                content: "x".into(),
                encoding: Default::default(),
                expect_sha256: None,
            },
        )
        .await;
    assert!(write.is_err(), "{write:?}");
    assert!(!env.home().join("planted.txt").exists());
    assert!(denied.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}

#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn shells_and_jobs_run_under_the_workspace_profile() {
    let env = env().await;
    let denied = deny_everything(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    session.shell(&client, "s").await;
    let (exit, output, _) = session
        .bash(
            &client,
            "c1",
            "s",
            "cd sub && export X=kept && echo ok > made.txt && cat made.txt",
        )
        .await;
    assert_eq!((exit, output.as_str()), (Some(0), "ok"), "{}", env.log());
    let (_, output, _) = session.bash(&client, "c2", "s", "echo $X; pwd").await;
    assert!(
        output.starts_with("kept\n") && output.ends_with("/project/sub"),
        "{output}"
    );
    let (exit, _, _) = session.bash(&client, "c3", "s", "cat ~/secret.txt").await;
    assert_ne!(exit, Some(0), "the shell read outside the workspace");
    let (exit, _, _) = session
        .bash(&client, "c4", "s", "echo x > ~/planted.txt")
        .await;
    assert_ne!(exit, Some(0));
    assert!(!env.home().join("planted.txt").exists());

    // Interrupt still reaches the command under nono.
    let result = session
        .run(
            &client,
            "c5",
            crate::harness::bash_op("s", "sleep 1000", Some(1)),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            result,
            HostResult::Bash {
                timed_out: true,
                ..
            }
        ),
        "{result:?}"
    );

    // Waiting for input is seen through nono, and the user answers in the terminal.
    let asking = client
        .send::<methods::ToolRun>(session.run_params(
            "ask",
            crate::harness::bash_op("s", "read -r answer; echo got:$answer", None),
        ))
        .await;
    let waiting = client.note("tool.waiting_input").await;
    assert_eq!(waiting["waiting"], true);
    crate::harness::type_into(&env, &format!("{}/s", session.id), b"yes\n");
    let answered: HostResult = serde_json::from_value(asking.await.unwrap().unwrap()).unwrap();
    assert!(
        matches!(answered, HostResult::Bash { waited_for_input: true, ref output, .. } if output.ends_with("got:yes")),
        "{answered:?}"
    );

    // A job under the same profile.
    let subscribed = client
        .call::<methods::JobsSubscribe>(JobsSubscribeParams { since: None })
        .await
        .unwrap();
    drop(subscribed);
    let job = session
        .run(
            &client,
            "j1",
            HostOp::JobRun {
                command: "echo job-ok; cat ~/secret.txt".into(),
                cwd: None,
                name: None,
                timeout_s: None,
                follow: None,
            },
        )
        .await
        .unwrap();
    let HostResult::JobRun { job } = job else {
        panic!()
    };
    let log = std::path::PathBuf::from(job.log_path.as_str());
    crate::harness::wait_until("the job's output", || {
        std::fs::read_to_string(&log).is_ok_and(|l| l.contains("job-ok"))
    })
    .await;
    loop {
        let note = client.note("jobs.event").await;
        let exit = note["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "exit")
            .cloned();
        if let Some(exit) = exit {
            assert_ne!(exit["exit_code"], 0, "the job read outside the workspace");
            break;
        }
    }
    assert!(
        !std::fs::read_to_string(&log)
            .unwrap()
            .contains("outside the workspace\n")
    );
    assert!(denied.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}

/// The user's sandboxed terminal is confined like the agent's shells (its approvals are marked
/// as the user's); an unsandboxed one isn't.
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn user_terminals_are_confined_unless_unsandboxed() {
    use controlroom_protocol::session::ShellSandbox;

    let env = env().await;
    let approver = env.controller().await;
    approver
        .call::<methods::ApprovalsSubscribe>(NoParams {})
        .await
        .unwrap();
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    let sandboxed = session
        .user_shell(&client, "you@devbox", ShellSandbox::Workspace)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", env.log()));
    let unsandboxed = session
        .user_shell(&client, "you@devbox-2", ShellSandbox::Unsandboxed)
        .await
        .unwrap();
    let probe = |name: &str| {
        format!(
            "cat ~/secret.txt > {name}-read.txt 2>&1; echo x > ~/{name}-planted.txt; echo $? > {name}-done.txt\r"
        )
    };

    // The sandboxed terminal asks to read outside the workspace: an approval marked as the
    // user's, denied.
    crate::harness::type_into(&env, sandboxed.shell_id.as_str(), probe("in").as_bytes());
    let note = approver.note("approval.request").await;
    let approval: HostApproval = serde_json::from_value(note["approval"].clone()).unwrap();
    assert!(approval.from_user_terminal, "{approval:?}");
    approver
        .call::<methods::ApprovalRespond>(ApprovalRespondParams {
            approval_id: approval.id,
            option_id: "deny".into(),
            reason: None,
        })
        .await
        .unwrap();
    // Later approvals of the same command (the write) are denied too.
    tokio::spawn(async move {
        loop {
            let note = approver.note("approval.request").await;
            let approval: HostApproval = serde_json::from_value(note["approval"].clone()).unwrap();
            let _ = approver
                .call::<methods::ApprovalRespond>(ApprovalRespondParams {
                    approval_id: approval.id,
                    option_id: "deny".into(),
                    reason: None,
                })
                .await;
        }
    });
    let done = env.project().join("in-done.txt");
    crate::harness::wait_until("the sandboxed probe ran", || done.exists()).await;
    let read = std::fs::read_to_string(env.project().join("in-read.txt")).unwrap();
    assert!(!read.contains("outside the workspace"), "{read}");
    assert!(!env.home().join("in-planted.txt").exists());

    // The unsandboxed one reads and writes as the user.
    crate::harness::type_into(&env, unsandboxed.shell_id.as_str(), probe("out").as_bytes());
    let done = env.project().join("out-done.txt");
    crate::harness::wait_until("the unsandboxed probe ran", || done.exists()).await;
    let read = std::fs::read_to_string(env.project().join("out-read.txt")).unwrap();
    assert_eq!(read, "outside the workspace\n");
    assert!(env.home().join("out-planted.txt").exists());
}

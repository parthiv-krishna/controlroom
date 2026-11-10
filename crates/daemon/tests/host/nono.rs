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
use controlroom_protocol::session::{RequestBody, SandboxLevel, SandboxSpec};

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

/// Denies every sandbox approval and records it.
async fn record_and_deny(env: &HostEnv) -> Arc<std::sync::Mutex<Vec<HostApproval>>> {
    let approver = env.controller().await;
    approver
        .call::<methods::ApprovalsSubscribe>(NoParams {})
        .await
        .unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = seen.clone();
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
                    option_id: "deny".into(),
                    reason: None,
                })
                .await;
            record.lock().unwrap().push(approval);
        }
    });
    seen
}

/// The paths approvals were raised for.
fn asked(seen: &std::sync::Mutex<Vec<HostApproval>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|a| match &a.body {
            RequestBody::Path { path, .. } => Some(path.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// A strict shell reads the runtime probes (`/proc/sys`, transparent huge pages, its cgroup
/// limits) without an approval.
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn runtime_probes_are_readable_without_asking() {
    let env = env().await;
    let seen = record_and_deny(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    session.shell(&client, "s").await;
    let probes = "cat /proc/sys/vm/nr_hugepages /proc/sys/vm/overcommit_memory \
                  /sys/kernel/mm/transparent_hugepage/enabled >/dev/null && \
                  cat /sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/memory.max >/dev/null && \
                  echo probed";
    let (exit, output, _) = session.bash(&client, "c1", "s", probes).await;
    assert_eq!(
        (exit, output.as_str()),
        (Some(0), "probed"),
        "{}",
        env.log()
    );
    let asked = asked(&seen);
    assert!(
        !asked
            .iter()
            .any(|p| p.starts_with("/proc/sys") || p.starts_with("/sys")),
        "{asked:?}"
    );
    let (exit, _, _) = session
        .bash(&client, "c2", "s", "echo 0 > /proc/sys/vm/nr_hugepages")
        .await;
    assert_ne!(exit, Some(0), "/proc/sys is read-only");
}

/// A never-path fails without an approval (nono denies a supervised open with EPERM, "Operation
/// not permitted", whoever decides; Landlock's own denials are EACCES); a granted `~/.cache/x` is
/// writable without one. Both hold in the agent's shell and in the user's sandboxed terminal.
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn never_paths_are_denied_silently_and_grants_need_no_approval() {
    use controlroom_protocol::ids::GrantId;
    use controlroom_protocol::session::{GrantKind, GrantScope, PathAccess, SandboxGrant};

    let env = env().await;
    std::fs::create_dir_all(env.home().join(".cache/x")).unwrap();
    let seen = record_and_deny(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let spec = SandboxSpec {
        grants: vec![SandboxGrant {
            id: GrantId::generate(),
            kind: GrantKind::Path,
            value: "~/.cache/x".into(),
            access: Some(PathAccess::ReadWrite),
            scope: GrantScope::Standing,
            granted_at: controlroom_protocol::UnixMillis(1),
            by: None,
        }],
        never: vec!["~/secret.txt".into()],
        ..crate::harness::sandbox(SandboxLevel::Strict)
    };
    let session = Session::open_with(&env, &client, spec).await;
    session.shell(&client, "s").await;
    let (exit, output, _) = session
        .bash(&client, "c1", "s", "cat ~/secret.txt 2>&1")
        .await;
    assert_ne!(exit, Some(0));
    assert!(output.contains("Operation not permitted"), "{output}");
    let (exit, output, _) = session
        .bash(
            &client,
            "c2",
            "s",
            "echo cached > ~/.cache/x/f && cat ~/.cache/x/f",
        )
        .await;
    assert_eq!(
        (exit, output.as_str()),
        (Some(0), "cached"),
        "{}",
        env.log()
    );
    // The user's sandboxed terminal runs under the workspace's user profile: the same grants,
    // runtime reads and never-list.
    let terminal = session
        .user_shell(
            &client,
            "you@devbox",
            controlroom_protocol::session::ShellSandbox::Workspace,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", env.log()));
    crate::harness::type_into(
        &env,
        terminal.shell_id.as_str(),
        b"cat ~/secret.txt > u-read.txt 2>&1; echo mine > ~/.cache/x/g; \
          cat /proc/sys/vm/nr_hugepages > u-probe.txt; echo $? > u-done.txt\r",
    );
    let done = env.project().join("u-done.txt");
    crate::harness::wait_until("the terminal's probe ran", || done.exists()).await;
    let read = std::fs::read_to_string(env.project().join("u-read.txt")).unwrap();
    assert!(read.contains("Operation not permitted"), "{read}");
    assert_eq!(
        std::fs::read_to_string(env.home().join(".cache/x/g")).unwrap(),
        "mine\n"
    );
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "0\n");
    let asked = asked(&seen);
    let home = env.home().to_str().unwrap().to_owned();
    assert!(
        !asked
            .iter()
            .any(|p| p.starts_with(&format!("{home}/secret.txt"))
                || p.starts_with(&format!("{home}/.cache/x"))),
        "{asked:?}"
    );
}

/// A level change (the workspace reopened at another level) restarts the agent's shell under
/// the new profile, and the restarted shell is confined accordingly: strict asks for (and here
/// gets denied) home config and other files outside the workspace, relaxed reads the configured
/// home config without asking, off reads anything, and strict again confines it again.
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn a_shell_restarted_at_another_level_is_confined_by_it() {
    let nono = std::env::var("CR_TEST_NONO").unwrap_or_else(|_| "nono".into());
    let env = env_with(
        &format!(
            "nono = {nono:?}\nrelaxed_reads = [{{ path = \"~/.gitconfig\", kind = \"file\" }}]\n"
        ),
        |env| {
            std::fs::write(env.home().join(".gitconfig"), "[user]\n\tname = alice\n").unwrap();
        },
    )
    .await;
    let seen = record_and_deny(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    session.shell(&client, "s").await;
    let reopen = |level| {
        client.call::<methods::WorkspaceOpen>(controlroom_protocol::host::WorkspaceOpenParams {
            workspace_id: session.workspace,
            session_id: session.id,
            path: "~/project".into(),
            sandbox: crate::harness::sandbox(level),
        })
    };
    let read = |call: &'static str, file: &'static str| {
        let session = &session;
        let client = &client;
        async move {
            let (exit, _, result) = session
                .bash(client, call, "s", &format!("cat ~/{file} > /dev/null"))
                .await;
            let HostResult::Bash { shell_reset, .. } = result else {
                unreachable!()
            };
            (exit == Some(0), shell_reset)
        }
    };

    // Strict: both files are outside the workspace; reading asks (and is denied).
    assert_eq!(read("c1", ".gitconfig").await, (false, None));
    assert_eq!(asked(&seen).len(), 1, "{:?}", asked(&seen));

    // Relaxed: the shell restarts; home config is readable without asking, the rest isn't.
    reopen(SandboxLevel::Relaxed).await.unwrap();
    let reset = Some("the sandbox level changed".to_owned());
    assert_eq!(read("c2", ".gitconfig").await, (true, reset.clone()));
    assert_eq!(asked(&seen).len(), 1, "{:?}", asked(&seen));
    assert_eq!(read("c3", "secret.txt").await, (false, None));
    assert_eq!(asked(&seen).len(), 2, "{:?}", asked(&seen));

    // Off: anything, without asking.
    reopen(SandboxLevel::Off).await.unwrap();
    assert_eq!(read("c4", "secret.txt").await, (true, reset.clone()));
    assert_eq!(asked(&seen).len(), 2, "{:?}", asked(&seen));

    // Strict again: confined again.
    reopen(SandboxLevel::Strict).await.unwrap();
    assert_eq!(read("c5", "secret.txt").await, (false, reset));
    assert_eq!(read("c6", ".gitconfig").await, (false, None));
    assert_eq!(asked(&seen).len(), 4, "{:?}", asked(&seen));
}

/// A standing grant of `kind` (`~` allowed) for the tests below.
fn standing(
    kind: controlroom_protocol::session::GrantKind,
    value: &str,
    read: bool,
) -> controlroom_protocol::session::SandboxGrant {
    use controlroom_protocol::session::{GrantScope, PathAccess, SandboxGrant};
    SandboxGrant {
        id: controlroom_protocol::ids::GrantId::generate(),
        kind,
        value: value.into(),
        access: read.then_some(PathAccess::Read),
        scope: GrantScope::Standing,
        granted_at: controlroom_protocol::UnixMillis(1),
        by: None,
    }
}

/// A read grant of `~/.bash_history`, a file nono 0.80 denies by default, takes effect: the
/// agent's interactive shell loads the history at startup and nothing is asked. Before the
/// profile listed granted denied paths in `filesystem.bypass_protection`, nono dropped the file
/// grant and every new shell asked "Read ~/.bash_history?".
#[tokio::test]
#[ignore = "needs nono 0.80 and Landlock (CR_TEST_NONO)"]
async fn a_read_grant_of_a_file_nono_denies_by_default_needs_no_approval() {
    use controlroom_protocol::session::GrantKind;

    let env = env().await;
    std::fs::write(env.home().join(".bash_history"), "echo hist-line\n").unwrap();
    let seen = record_and_deny(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let spec = SandboxSpec {
        grants: vec![standing(GrantKind::Path, "~/.bash_history", true)],
        ..crate::harness::sandbox(SandboxLevel::Strict)
    };
    let session = Session::open_with(&env, &client, spec).await;
    session.shell(&client, "s").await;
    let (exit, output, _) = session
        .bash(
            &client,
            "c1",
            "s",
            "history | grep -c hist-line; cat ~/.bash_history",
        )
        .await;
    assert_eq!(exit, Some(0), "{output}\n{}", env.log());
    assert!(
        output.starts_with("1\n") && output.ends_with("echo hist-line"),
        "{output}"
    );
    let asked = asked(&seen);
    assert!(
        !asked.iter().any(|p| p.ends_with("/.bash_history")),
        "{asked:?}"
    );
}

/// A socket grant lets the sandbox connect to that socket and no other (`af_unix_mediation:
/// pathname`). nono can't ask about a socket connect: the other fails at once.
#[tokio::test]
#[ignore = "needs nono 0.80, Landlock and curl (CR_TEST_NONO)"]
async fn a_socket_grant_lets_a_connect_through() {
    use controlroom_protocol::session::GrantKind;
    use std::os::unix::net::UnixListener;

    let env = env().await;
    let curl = which("curl");
    let sockets = env.dir.path().join("s");
    std::fs::create_dir(&sockets).unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for name in ["granted.sock", "other.sock"] {
        let listener = UnixListener::bind(sockets.join(name)).unwrap();
        let count = accepted.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                answer_http(&mut conn);
            }
        });
    }
    let granted = sockets.join("granted.sock");
    let other = sockets.join("other.sock");
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let spec = SandboxSpec {
        grants: vec![standing(
            GrantKind::Socket,
            granted.to_str().unwrap(),
            false,
        )],
        ..crate::harness::sandbox(SandboxLevel::Strict)
    };
    let session = Session::open_with(&env, &client, spec).await;
    session.shell(&client, "s").await;
    let connect = |socket: &std::path::Path| {
        format!(
            "{} -s --unix-socket {} http://localhost/ >/dev/null; echo $?",
            curl.display(),
            socket.display()
        )
    };
    // curl: 7 is "couldn't connect".
    let (_, output, _) = session.bash(&client, "c1", "s", &connect(&granted)).await;
    assert_eq!(output, "0", "{}", env.log());
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (_, output, _) = session.bash(&client, "c2", "s", &connect(&other)).await;
    assert_eq!(output, "7");
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// With the D-Bus system bus granted, a libdbus client (`dbus-send`) talks to it; without, it
/// can't connect. (systemd 254 and later bind their bus clients to an abstract-namespace name,
/// which nono's pathname mediation refuses, so `systemctl` and `busctl` fail either way; see
/// spec §11.6.) Skipped where the machine has no system bus or no `dbus-send`.
#[tokio::test]
#[ignore = "needs nono 0.80, Landlock, a system bus and dbus-send (CR_TEST_NONO)"]
async fn the_system_bus_answers_once_its_socket_is_granted() {
    use controlroom_protocol::session::GrantKind;

    const BUS: &str = "/run/dbus/system_bus_socket";
    if !std::path::Path::new(BUS).exists() {
        eprintln!("skipped: no system bus at {BUS}");
        return;
    }
    let env = env().await;
    let dbus_send = which("dbus-send");
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let call = format!(
        "{} --system --print-reply --dest=org.freedesktop.DBus / org.freedesktop.DBus.GetId \
         >/dev/null 2>&1; echo $?",
        dbus_send.display()
    );
    let spec = SandboxSpec {
        grants: vec![standing(GrantKind::Socket, BUS, false)],
        ..crate::harness::sandbox(SandboxLevel::Strict)
    };
    let granted = Session::open_with(&env, &client, spec).await;
    granted.shell(&client, "s").await;
    let (_, output, _) = granted.bash(&client, "c1", "s", &call).await;
    assert_eq!(output, "0", "{}", env.log());
    let plain = Session::open(&env, &client, SandboxLevel::Strict).await;
    plain.shell(&client, "s").await;
    let (_, output, _) = plain.bash(&client, "c2", "s", &call).await;
    assert_ne!(output, "0");
}

/// `systemctl status` raises no card for another process's `/proc` entry (`/proc/1/root`; the
/// host denies those without asking, since nono never opens them), and its `O_PATH` probe of
/// `/proc/` is asked as a read, not "read and write" (the static musl build needs
/// nix/nono-musl-o-path-access.patch). Skipped without `systemctl`.
#[tokio::test]
#[ignore = "needs nono 0.80, Landlock and systemctl (CR_TEST_NONO)"]
async fn proc_probes_ask_for_reads_and_never_for_other_processes() {
    let Some(systemctl) = find("systemctl") else {
        eprintln!("skipped: no systemctl");
        return;
    };
    let env = env().await;
    let seen = record_and_deny(&env).await;
    let (client, _) = env
        .connect(controlroom_protocol::agentd::ClientRole::Debug)
        .await;
    let session = Session::open(&env, &client, SandboxLevel::Strict).await;
    session.shell(&client, "s").await;
    let command = format!(
        "{} status --no-pager >/dev/null 2>&1; echo done",
        systemctl.display()
    );
    let (_, output, _) = session.bash(&client, "c1", "s", &command).await;
    assert_eq!(output, "done");
    let approvals = seen.lock().unwrap().clone();
    let paths: Vec<(String, controlroom_protocol::session::PathAccess)> = approvals
        .iter()
        .filter_map(|a| match &a.body {
            RequestBody::Path { path, access, .. } => Some((path.as_str().to_owned(), *access)),
            _ => None,
        })
        .collect();
    assert!(
        paths
            .iter()
            .any(|(p, _)| p.trim_end_matches('/') == "/proc"),
        "systemctl probes /proc: {paths:?}"
    );
    for (path, access) in &paths {
        let per_process = path
            .strip_prefix("/proc/")
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
        assert!(!per_process, "asked for {path}");
        if path.starts_with("/proc") {
            assert_eq!(
                *access,
                controlroom_protocol::session::PathAccess::Read,
                "{path}"
            );
        }
    }
}

/// Reads an HTTP request head from `conn` and answers 200.
fn answer_http(conn: &mut std::os::unix::net::UnixStream) {
    use std::io::{Read, Write};
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match conn.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let _ = conn.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
}

/// `name` on the test's `PATH`, resolved to its real file (the sandbox can't follow links
/// through an unreadable home).
fn find(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .and_then(|p| std::fs::canonicalize(p).ok())
}

fn which(name: &str) -> std::path::PathBuf {
    find(name).unwrap_or_else(|| panic!("{name} is not on PATH"))
}

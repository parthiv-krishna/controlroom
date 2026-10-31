//! Bootstrap tests: probe, install with verify-run, upgrade with rollback,
//! stop confirmation, pruning, `authorized_keys` edits and key rotation. They
//! run against a temp `$HOME`, locally and over the in-process SSH server.
#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use controlroom_client_core::SshPrivateKey;
use controlroom_client_core::bootstrap::{
    BootstrapError, DaemonBinarySource, InMemoryBinaries, InstallDecision, RemoteShell,
    UpgradeOutcome, Version, add_authorized_key, daemon_command, install, probe, prune_versions,
    read_current_link, remove_authorized_key, rollback, rotate_key, stop_daemon, upgrade,
};
use controlroom_client_core::ssh::{
    CommandOutput, ConnectOptions, SshError, SshErrorKind, connect,
};
use support::{Fake, LocalShell, Reaper, TestServer, base, fake_daemon, start_daemon_command};

const STOP_WAIT: Duration = Duration::from_secs(10);

fn source(fake: &Fake) -> InMemoryBinaries {
    let binary = fake.binary();
    InMemoryBinaries {
        version: Version::parse(fake.version).unwrap(),
        x86_64: Some(binary.clone()),
        aarch64: Some(binary),
    }
}

fn good(version: &str) -> InMemoryBinaries {
    source(&Fake::good(version))
}

fn local(home: &Path) -> LocalShell {
    LocalShell {
        home: home.to_owned(),
        path: None,
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

async fn current<S: RemoteShell>(shell: &S) -> Option<String> {
    read_current_link(shell).await.unwrap()
}

/// The health check `upgrade` runs: start the new daemon and say hello.
async fn start_and_hello<S: RemoteShell>(shell: &S) -> Result<String, String> {
    let start = shell
        .run(
            &start_daemon_command(".local/share/controlroom/current/controlroomd", "serve"),
            b"",
        )
        .await
        .map_err(|e| e.to_string())?;
    if !start.status.success() {
        return Err(start.stderr_str());
    }
    let hello = shell
        .run(&daemon_command(&["hello"]), b"")
        .await
        .map_err(|e| e.to_string())?;
    if hello.status.success() {
        Ok(hello.stdout_str().trim().to_owned())
    } else {
        Err(hello.stderr_str().trim().to_owned())
    }
}

#[tokio::test]
async fn probe_install_upgrade_and_never_downgrade() {
    let home = tempfile::tempdir().unwrap();
    let shell = local(home.path());

    let p = probe(&shell).await.unwrap();
    assert_eq!(p.home, home.path().to_str().unwrap());
    assert_eq!(p.daemon_version_output, None);
    let v1 = good("0.1.0");
    assert_eq!(p.decide(v1.version()), InstallDecision::Install);

    let installed = install(&shell, &v1, &p).await.unwrap();
    assert_eq!(installed.hashed_with, "sha256sum");
    assert_eq!(installed.previous, None);
    assert_eq!(installed.target(), "versions/0.1.0");
    let bin = base(home.path()).join("versions/0.1.0/controlroomd");
    assert_eq!(installed.path, bin.to_str().unwrap());
    assert_eq!(std::fs::read(&bin).unwrap(), fake_daemon("0.1.0"));
    let mode = std::fs::metadata(&bin).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.1.0"));

    let p = probe(&shell).await.unwrap();
    assert_eq!(p.installed_version(), Some(Version::new(0, 1, 0)));
    assert_eq!(p.current_link.as_deref(), Some("versions/0.1.0"));
    assert_eq!(p.decide(v1.version()), InstallDecision::UpToDate);

    // An upgrade keeps the old version (supervisors may run from it) and
    // reports it as the rollback target.
    let installed = install(&shell, &good("0.2.0"), &p).await.unwrap();
    assert_eq!(installed.previous.as_deref(), Some("versions/0.1.0"));
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.2.0"));
    assert!(bin.exists());

    // Never downgrade.
    let p = probe(&shell).await.unwrap();
    let err = install(&shell, &v1, &p).await.unwrap_err();
    assert!(
        matches!(err, BootstrapError::WouldDowngrade { .. }),
        "{err}"
    );
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.2.0"));

    // No temp files left behind.
    assert_eq!(names(&base(home.path())), ["current", "versions"]);
    assert_eq!(
        names(&base(home.path()).join("versions")),
        ["0.1.0", "0.2.0"]
    );
    assert_eq!(
        names(&base(home.path()).join("versions/0.2.0")),
        ["controlroomd"]
    );
}

#[tokio::test]
async fn a_binary_that_fails_its_verify_run_never_becomes_current() {
    let home = tempfile::tempdir().unwrap();
    let shell = local(home.path());
    install(&shell, &good("0.1.0"), &probe(&shell).await.unwrap())
        .await
        .unwrap();

    let crashing = Fake {
        version_output: None,
        ..Fake::good("0.2.0")
    };
    let mislabeled = Fake {
        version_output: Some("controlroomd 0.1.9".into()),
        ..Fake::good("0.2.0")
    };
    let wrong_arch = InMemoryBinaries {
        version: Version::new(0, 2, 0),
        x86_64: Some(b"\x7fELF not really an executable".to_vec()),
        aarch64: None,
    };
    let cases = [
        ("crashing", source(&crashing)),
        ("mislabeled", source(&mislabeled)),
        ("wrong arch", wrong_arch),
    ];
    for (what, src) in cases {
        let p = probe(&shell).await.unwrap();
        let err = install(&shell, &src, &p).await.unwrap_err();
        let BootstrapError::Verification(msg) = &err else {
            panic!("{what}: {err:?}")
        };
        assert!(msg.contains("verify-run"), "{what}: {msg}");
        assert_eq!(
            current(&shell).await.as_deref(),
            Some("versions/0.1.0"),
            "{what}"
        );
        // Neither the binary nor its temp file nor an empty directory remains.
        assert_eq!(
            names(&base(home.path()).join("versions")),
            ["0.1.0"],
            "{what}"
        );
    }
}

/// Corrupts every upload in transit: flips one byte, or drops the tail.
struct Corrupting {
    inner: LocalShell,
    truncate: bool,
}

impl RemoteShell for Corrupting {
    async fn run(&self, command: &str, stdin: &[u8]) -> Result<CommandOutput, SshError> {
        let mut stdin = stdin.to_vec();
        if self.truncate {
            stdin.truncate(stdin.len() / 2);
        } else if let Some(b) = stdin.last_mut() {
            *b ^= 1;
        }
        self.inner.run(command, &stdin).await
    }
}

#[tokio::test]
async fn corrupted_uploads_are_rejected() {
    for (truncate, expected) in [(false, "sha256 mismatch"), (true, "size mismatch")] {
        let home = tempfile::tempdir().unwrap();
        let shell = Corrupting {
            inner: local(home.path()),
            truncate,
        };
        let p = probe(&shell.inner).await.unwrap();
        let err = install(&shell, &good("0.1.0"), &p).await.unwrap_err();
        assert!(
            matches!(&err, BootstrapError::Verification(m) if m.contains(expected)),
            "{err:?}"
        );
        assert_eq!(current(&shell.inner).await, None);
        assert!(!base(home.path()).join("versions/0.1.0").exists());
    }
}

/// A PATH with only the tools the scripts need, minus every sha256 tool.
fn path_without_sha256(dir: &Path) -> String {
    for tool in [
        "mkdir", "cat", "wc", "tr", "chmod", "cut", "mv", "rm", "rmdir", "ln", "readlink", "head",
        "uname", "sh",
    ] {
        std::os::unix::fs::symlink(support::which(tool), dir.join(tool)).unwrap();
    }
    dir.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn install_without_a_hash_tool_still_verify_runs() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let shell = LocalShell {
        home: home.path().to_owned(),
        path: Some(path_without_sha256(bin.path())),
    };
    let p = probe(&shell).await.unwrap();
    let installed = install(&shell, &good("0.3.0"), &p).await.unwrap();
    assert_eq!(installed.hashed_with, "none");

    let mislabeled = Fake {
        version_output: Some("controlroomd 0.3.9".into()),
        ..Fake::good("0.4.0")
    };
    let p = probe(&shell).await.unwrap();
    let err = install(&shell, &source(&mislabeled), &p).await.unwrap_err();
    assert!(matches!(err, BootstrapError::Verification(_)), "{err}");
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.3.0"));
}

#[tokio::test]
async fn rollback_restores_the_previous_target() {
    let home = tempfile::tempdir().unwrap();
    let shell = local(home.path());
    let first = install(&shell, &good("0.1.0"), &probe(&shell).await.unwrap())
        .await
        .unwrap();
    let second = install(&shell, &good("0.2.0"), &probe(&shell).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        rollback(&shell, &second).await.unwrap().as_deref(),
        Some("versions/0.1.0")
    );
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.1.0"));
    // `current` no longer points at the install being undone: refuse.
    let err = rollback(&shell, &second).await.unwrap_err();
    assert!(err.to_string().contains("not rolling back"), "{err}");
    // Undoing a fresh install removes `current`.
    assert_eq!(rollback(&shell, &first).await.unwrap(), None);
    assert_eq!(current(&shell).await, None);
}

#[tokio::test]
async fn upgrade_swaps_stops_the_old_daemon_and_checks_the_new_one() {
    let home = tempfile::tempdir().unwrap();
    let reaper = Reaper(home.path().to_owned());
    let shell = local(home.path());

    let outcome = upgrade(&shell, &good("0.1.0"), STOP_WAIT, || {
        start_and_hello(&shell)
    })
    .await
    .unwrap();
    let UpgradeOutcome::Upgraded {
        installed,
        stopped,
        checked,
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!((installed.previous, stopped), (None, 0));
    assert_eq!(checked, "hello from 0.1.0");
    let base = base(home.path());
    let serve = |p: &str| format!("{}/{p}/controlroomd serve", base.display());
    assert!(reaper.running(&serve("current")));

    let outcome = upgrade(&shell, &good("0.2.0"), STOP_WAIT, || {
        start_and_hello(&shell)
    })
    .await
    .unwrap();
    let UpgradeOutcome::Upgraded {
        stopped, checked, ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    // The old daemon was stopped (and confirmed gone) before the new one started.
    assert_eq!((stopped, checked.as_str()), (1, "hello from 0.2.0"));
    assert_eq!(reaper.pids().len(), 1, "only the new daemon runs");
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.2.0"));

    let outcome = upgrade(&shell, &good("0.2.0"), STOP_WAIT, || async {
        Err::<(), _>("must not run")
    })
    .await
    .unwrap();
    assert!(matches!(outcome, UpgradeOutcome::UpToDate));
    stop_daemon(&shell, STOP_WAIT).await.unwrap();
}

#[tokio::test]
async fn a_failed_hello_rolls_back_and_stops_the_new_daemon() {
    let home = tempfile::tempdir().unwrap();
    let reaper = Reaper(home.path().to_owned());
    let shell = local(home.path());
    upgrade(&shell, &good("0.1.0"), STOP_WAIT, || {
        start_and_hello(&shell)
    })
    .await
    .unwrap();

    let incompatible = Fake {
        hello_ok: false,
        ..Fake::good("0.2.0")
    };
    let err = upgrade(&shell, &source(&incompatible), STOP_WAIT, || {
        start_and_hello(&shell)
    })
    .await
    .unwrap_err();
    let BootstrapError::RolledBack { reason, restored } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(reason, "incompatible");
    assert_eq!(restored.as_deref(), Some("versions/0.1.0"));
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.1.0"));
    assert!(reaper.pids().is_empty(), "the new daemon was stopped");

    // The next connect runs the old daemon again.
    assert_eq!(start_and_hello(&shell).await.unwrap(), "hello from 0.1.0");
    stop_daemon(&shell, STOP_WAIT).await.unwrap();
}

#[tokio::test]
async fn an_old_daemon_that_does_not_stop_rolls_the_upgrade_back() {
    let home = tempfile::tempdir().unwrap();
    let reaper = Reaper(home.path().to_owned());
    let shell = local(home.path());
    let stubborn = Fake {
        obeys_term: false,
        ..Fake::good("0.1.0")
    };
    upgrade(&shell, &source(&stubborn), STOP_WAIT, || {
        start_and_hello(&shell)
    })
    .await
    .unwrap();

    let checked = AtomicUsize::new(0);
    let err = upgrade(
        &shell,
        &good("0.2.0"),
        Duration::from_millis(300),
        || async {
            checked.fetch_add(1, Ordering::Relaxed);
            Ok::<_, String>(())
        },
    )
    .await
    .unwrap_err();
    let BootstrapError::RolledBack { reason, restored } = &err else {
        panic!("{err:?}")
    };
    assert!(reason.contains("still running"), "{reason}");
    assert_eq!(restored.as_deref(), Some("versions/0.1.0"));
    assert_eq!(
        checked.load(Ordering::Relaxed),
        0,
        "no check while the old daemon runs"
    );
    assert_eq!(current(&shell).await.as_deref(), Some("versions/0.1.0"));
    assert_eq!(reaper.pids().len(), 1);
}

#[tokio::test]
async fn stop_daemon_signals_only_serve_and_confirms_exit() {
    let home = tempfile::tempdir().unwrap();
    let reaper = Reaper(home.path().to_owned());
    let shell = local(home.path());
    install(&shell, &good("0.1.0"), &probe(&shell).await.unwrap())
        .await
        .unwrap();
    let bin = ".local/share/controlroom/versions/0.1.0/controlroomd";
    for sub in ["serve", "supervise"] {
        let out = shell
            .run(&start_daemon_command(bin, sub), b"")
            .await
            .unwrap();
        assert!(out.status.success(), "{}", out.stderr_str());
    }
    let path = base(home.path()).join("versions/0.1.0/controlroomd");
    let cmd = |sub: &str| format!("{} {sub}", path.display());
    assert!(reaper.running(&cmd("serve")));

    assert_eq!(stop_daemon(&shell, STOP_WAIT).await.unwrap(), 1);
    assert!(!reaper.running(&cmd("serve")));
    assert!(reaper.running(&cmd("supervise")), "supervisors survive");
    assert_eq!(stop_daemon(&shell, STOP_WAIT).await.unwrap(), 0);

    // A daemon that ignores SIGTERM is reported, not silently assumed gone.
    let stubborn = Fake {
        obeys_term: false,
        ..Fake::good("0.2.0")
    };
    install(&shell, &source(&stubborn), &probe(&shell).await.unwrap())
        .await
        .unwrap();
    let new_bin = ".local/share/controlroom/versions/0.2.0/controlroomd";
    shell
        .run(&start_daemon_command(new_bin, "serve"), b"")
        .await
        .unwrap();
    let err = stop_daemon(&shell, Duration::from_millis(200))
        .await
        .unwrap_err();
    let BootstrapError::DaemonStillRunning { pids } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(pids.len(), 1);
}

#[tokio::test]
async fn prune_keeps_current_kept_and_running_versions() {
    let home = tempfile::tempdir().unwrap();
    let _reaper = Reaper(home.path().to_owned());
    let shell = local(home.path());
    for v in ["0.1.0", "0.2.0", "0.3.0", "0.4.0"] {
        install(&shell, &good(v), &probe(&shell).await.unwrap())
            .await
            .unwrap();
    }
    // A detached supervisor still runs from 0.1.0.
    let out = shell
        .run(
            &start_daemon_command(
                ".local/share/controlroom/versions/0.1.0/controlroomd",
                "supervise",
            ),
            b"",
        )
        .await
        .unwrap();
    assert!(out.status.success(), "{}", out.stderr_str());

    let removed = prune_versions(&shell, &[Version::new(0, 3, 0)])
        .await
        .unwrap();
    assert_eq!(removed, ["0.2.0"]);
    assert_eq!(
        names(&base(home.path()).join("versions")),
        ["0.1.0", "0.3.0", "0.4.0"]
    );
}

#[tokio::test]
async fn authorized_keys_add_and_remove_preserve_mode() {
    let home = tempfile::tempdir().unwrap();
    let shell = local(home.path());
    let old = SshPrivateKey::generate_ed25519("controlroom@devbox").unwrap();
    let new = SshPrivateKey::generate_ed25519("controlroom@devbox").unwrap();
    let ssh_dir = home.path().join(".ssh");
    let file = ssh_dir.join("authorized_keys");

    // Creates ~/.ssh (0700) and the file (0600) if missing.
    assert!(
        add_authorized_key(&shell, &old.public_key_line())
            .await
            .unwrap()
    );
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&ssh_dir), 0o700);
    assert_eq!(mode(&file), 0o600);

    // Existing content and mode are kept.
    std::fs::write(
        &file,
        format!("# alice's keys\n{}\n", old.public_key_line()),
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();

    assert!(
        add_authorized_key(&shell, &new.public_key_line())
            .await
            .unwrap()
    );
    assert!(
        !add_authorized_key(&shell, &new.public_key_line())
            .await
            .unwrap()
    );
    assert!(
        remove_authorized_key(&shell, &old.public_key_line())
            .await
            .unwrap()
    );
    assert!(
        !remove_authorized_key(&shell, &old.public_key_line())
            .await
            .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("# alice's keys\n{}\n", new.public_key_line())
    );
    assert_eq!(mode(&file), 0o640);
    assert_eq!(names(&ssh_dir), ["authorized_keys"]);

    // A symlinked authorized_keys is managed elsewhere: refuse to replace it.
    let real = home.path().join("managed_keys");
    std::fs::write(&real, "").unwrap();
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&real, &file).unwrap();
    let err = add_authorized_key(&shell, &new.public_key_line())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
}

/// Edits authorized_keys behind the client's back between its read and write.
struct ConcurrentEditor {
    inner: LocalShell,
    calls: AtomicUsize,
}

impl RemoteShell for ConcurrentEditor {
    async fn run(&self, command: &str, stdin: &[u8]) -> Result<CommandOutput, SshError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 1 {
            let file = self.inner.home.join(".ssh/authorized_keys");
            let contents = std::fs::read_to_string(&file).unwrap();
            // Same length, different content: a size check would miss it.
            std::fs::write(&file, contents.replace("bob", "eve")).unwrap();
        }
        self.inner.run(command, stdin).await
    }
}

#[tokio::test]
async fn authorized_keys_edits_detect_concurrent_changes() {
    let home = tempfile::tempdir().unwrap();
    let key = SshPrivateKey::generate_ed25519("controlroom@devbox").unwrap();
    std::fs::create_dir(home.path().join(".ssh")).unwrap();
    let file = home.path().join(".ssh/authorized_keys");
    std::fs::write(&file, "# bob's keys\n").unwrap();
    let shell = ConcurrentEditor {
        inner: local(home.path()),
        calls: AtomicUsize::new(0),
    };
    let err = add_authorized_key(&shell, &key.public_key_line())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("changed while editing"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# eve's keys\n");
}

async fn ssh_setup() -> (TestServer, SshPrivateKey) {
    let key = SshPrivateKey::generate_ed25519("controlroom@devbox").unwrap();
    let server = TestServer::start(vec![]).await;
    let ssh = server.home_path().join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    std::fs::write(
        ssh.join("authorized_keys"),
        format!("{}\n", key.public_key_line()),
    )
    .unwrap();
    (server, key)
}

#[tokio::test]
async fn upgrade_over_ssh() {
    let (server, key) = ssh_setup().await;
    let _reaper = Reaper(server.home_path().to_owned());
    let conn = connect(
        &server.target(),
        &key,
        Some(&server.host_key),
        &ConnectOptions::default(),
    )
    .await
    .unwrap();
    let outcome = upgrade(&conn, &good("0.1.0"), STOP_WAIT, || start_and_hello(&conn))
        .await
        .unwrap();
    assert!(
        matches!(outcome, UpgradeOutcome::Upgraded { .. }),
        "{outcome:?}"
    );
    let out = conn
        .run(&daemon_command(&["--version"]), b"")
        .await
        .unwrap();
    assert_eq!(out.stdout_str(), "controlroomd 0.1.0\n");
    stop_daemon(&conn, STOP_WAIT).await.unwrap();
}

#[tokio::test]
async fn rotate_key_over_ssh() {
    let (server, old) = ssh_setup().await;
    let options = ConnectOptions::default();
    let conn = connect(&server.target(), &old, Some(&server.host_key), &options)
        .await
        .unwrap();
    let new = SshPrivateKey::generate_ed25519("controlroom@devbox").unwrap();

    let new_conn = rotate_key(
        &conn,
        &server.target(),
        &new,
        &old.public_key_line(),
        &options,
    )
    .await
    .unwrap();
    assert_eq!(new_conn.run("echo ok", b"").await.unwrap().stdout, b"ok\n");
    let contents =
        std::fs::read_to_string(server.home_path().join(".ssh/authorized_keys")).unwrap();
    assert_eq!(contents, format!("{}\n", new.public_key_line()));
    let err = connect(&server.target(), &old, Some(&server.host_key), &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), SshErrorKind::AuthFailed, "{err:?}");
    connect(&server.target(), &new, Some(&server.host_key), &options)
        .await
        .unwrap();
}

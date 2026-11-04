//! The controller's terminal channel: `controlroomd pty attach --framed` (attached frame,
//! output with positions, typing, in-band resize, foreground program, resume after a drop, the
//! end of the shell).

use std::process::Stdio;

use controlroom_protocol::host::terminal::{Attached, Decoder, Frame as _, FromHost, ToHost};
use controlroom_protocol::session::SandboxLevel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::harness::{HostEnv, Session, TIMEOUT};

/// A running `pty attach --framed`.
struct Framed {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    decoder: Decoder,
    /// Every output byte so far, and the last position.
    screen: String,
    last_seq: u64,
    foreground: Option<String>,
}

impl Framed {
    fn spawn(env: &HostEnv, shell: &str, extra: &[&str]) -> Self {
        let mut cmd = tokio::process::Command::from(env.command());
        cmd.args(["pty", "attach", shell, "--framed", "--state-dir"])
            .arg(env.state())
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        Self {
            stdin: child.stdin.take().unwrap(),
            stdout: child.stdout.take().unwrap(),
            child,
            decoder: Decoder::default(),
            screen: String::new(),
            last_seq: 0,
            foreground: None,
        }
    }

    /// The next frame (`None` at the end of the stream).
    async fn frame(&mut self) -> Option<FromHost> {
        loop {
            if let Some(frame) = self.decoder.next_frame::<FromHost>().unwrap() {
                match &frame {
                    FromHost::Output { seq, data } => {
                        assert!(*seq >= self.last_seq, "positions never go back");
                        self.last_seq = *seq;
                        self.screen.push_str(&String::from_utf8_lossy(data));
                    }
                    FromHost::Foreground(name) => self.foreground.clone_from(name),
                    _ => {}
                }
                return Some(frame);
            }
            let mut buf = [0; 4096];
            let n = tokio::time::timeout(TIMEOUT, self.stdout.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no frame in time; screen so far: {:?}", self.screen))
                .unwrap();
            if n == 0 {
                return None;
            }
            self.decoder.push(&buf[..n]);
        }
    }

    async fn attached(&mut self) -> Attached {
        match self.frame().await {
            Some(FromHost::Attached(attached)) => attached,
            other => panic!("expected attached, got {other:?}"),
        }
    }

    /// Reads frames until `pred` holds.
    async fn until(&mut self, what: &str, pred: impl Fn(&Self) -> bool) {
        while !pred(self) {
            assert!(
                self.frame().await.is_some(),
                "stream ended waiting for {what}"
            );
        }
    }

    async fn send(&mut self, frame: ToHost) {
        self.stdin.write_all(&frame.encode()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn type_line(&mut self, line: &str) {
        self.send(ToHost::Input(format!("{line}\r").into_bytes()))
            .await;
    }
}

#[tokio::test]
async fn a_framed_attach_types_resizes_reports_the_foreground_and_resumes() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    session.shell(&client, "s").await;
    let shell = format!("{}/s", session.id);
    session
        .bash(&client, "c1", "s", "echo agent-was-here")
        .await;

    let mut term = Framed::spawn(&env, &shell, &[]);
    let attached = term.attached().await;
    assert_eq!(attached.from_seq, 1, "a short history is replayed whole");
    term.until("the replayed agent command", |t| {
        t.screen.contains("agent-was-here")
    })
    .await;

    term.type_line("echo typed-$((6*7))").await;
    term.until("the typed command's output", |t| {
        t.screen.contains("typed-42")
    })
    .await;

    term.send(ToHost::Resize {
        cols: 101,
        rows: 37,
    })
    .await;
    term.type_line("stty size").await;
    term.until("the new size", |t| t.screen.contains("37 101"))
        .await;

    term.type_line("sleep 300").await;
    term.until("sleep in the foreground", |t| {
        t.foreground.as_deref() == Some("sleep")
    })
    .await;
    term.send(ToHost::Input(b"\x03".to_vec())).await;
    term.until("back at the prompt", |t| t.foreground.is_none())
        .await;

    // A second client resumes where the first one's output ended: nothing again, nothing lost.
    let resume_at = term.last_seq + 1;
    drop(term);
    let mut again = Framed::spawn(
        &env,
        &shell,
        &[
            "--since-seq",
            &resume_at.to_string(),
            "--instance",
            &attached.instance,
        ],
    );
    let resumed = again.attached().await;
    assert_eq!(resumed.instance, attached.instance);
    assert_eq!(resumed.from_seq, resume_at);
    again.type_line("echo resumed").await;
    again
        .until("output after the resume", |t| t.screen.contains("resumed"))
        .await;
    assert!(!again.screen.contains("typed-42"), "{:?}", again.screen);

    // Another instance (the shell was reopened) replays recent output instead.
    let mut other = Framed::spawn(
        &env,
        &shell,
        &[
            "--since-seq",
            "2",
            "--instance",
            "01JAM000000000000000000009",
        ],
    );
    assert_eq!(other.attached().await.from_seq, 1);
    drop(other);

    // The shell's process ends: the stream ends and the attach exits 0.
    again.type_line("exit").await;
    while again.frame().await.is_some() {}
    let status = again.child.wait().await.unwrap();
    assert!(status.success(), "{status}");

    // Nothing to attach to: a failure with the reason on stderr.
    let gone = Framed::spawn(&env, &format!("{}/nope", session.id), &[]);
    let out = gone.child.wait_with_output().await.unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no running shell"),
        "{out:?}"
    );
}

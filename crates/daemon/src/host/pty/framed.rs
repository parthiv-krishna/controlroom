//! `controlroomd pty attach --framed`: the controller's terminal channel
//! ([`controlroom_protocol::host::terminal`]).
//!
//! One task writes stdout: the `attached` frame, then every output record with its transcript
//! position, and the foreground program whenever it changes. It checks the foreground at most
//! every [`FOREGROUND_POLL`], and only after terminal activity (output or typing), from
//! `/proc`. Another task decodes stdin: input is typed into the terminal, resizes applied.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use controlroom_protocol::host::terminal::{Attached, Decoder, Frame as _, FromHost, ToHost};
use controlroom_supervisor::Client;
use controlroom_transcript::PtyChunk;
use tokio::io::{AsyncReadExt, AsyncWriteExt, Stdout};

use super::{ShellTarget, recent_start};
use crate::host::shells::proc;

/// How often the foreground program is checked while the terminal is active.
const FOREGROUND_POLL: Duration = Duration::from_millis(250);
/// The largest output chunk per frame (supervisor records are smaller).
const OUTPUT_CHUNK: usize = 64 * 1024;

/// Continue a stream after a link drop.
pub(super) struct Resume {
    /// The next transcript position the client needs.
    pub seq: u64,
    /// The shell instance the position belongs to.
    pub instance: Option<String>,
}

/// Attaches to `shell` until its process ends or stdin closes.
pub(super) async fn attach(shell: &ShellTarget, resume: Option<Resume>) -> anyhow::Result<()> {
    let reader = shell.connect().await?;
    let typist = shell.connect().await?;
    let instance = reader.hello().id.clone();
    let last_seq = reader.status().await?.last_seq;
    let from_seq = start(&instance, last_seq, resume);
    let mut stream = reader.read(from_seq).await?;
    let active = Arc::new(AtomicBool::new(true));

    let output = {
        let active = active.clone();
        async move {
            let mut out = tokio::io::stdout();
            send(
                &mut out,
                FromHost::Attached(Attached { instance, from_seq }),
            )
            .await?;
            let mut shown: Option<String> = None;
            let mut poll = tokio::time::interval(FOREGROUND_POLL);
            loop {
                tokio::select! {
                    record = stream.next() => {
                        let Some(record) = record else { return anyhow::Ok(()) };
                        let Some(chunk) = PtyChunk::from_record(&record) else { continue };
                        for data in chunk.0.chunks(OUTPUT_CHUNK) {
                            let frame = FromHost::Output { seq: record.seq, data: data.to_vec() };
                            send(&mut out, frame).await?;
                        }
                        active.store(true, Ordering::Relaxed);
                    }
                    _ = poll.tick() => {
                        if !active.swap(false, Ordering::Relaxed) {
                            continue;
                        }
                        let now = foreground(shell);
                        if now != shown {
                            send(&mut out, FromHost::Foreground(now.clone())).await?;
                            shown = now;
                        }
                    }
                }
            }
        }
    };
    let input = async {
        let mut stdin = tokio::io::stdin();
        let mut decoder = Decoder::default();
        let mut buf = vec![0; 16 * 1024];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                return anyhow::Ok(());
            }
            decoder.push(&buf[..n]);
            while let Some(frame) = decoder.next_frame::<ToHost>()? {
                apply(&typist, frame).await?;
                active.store(true, Ordering::Relaxed);
            }
        }
    };
    tokio::select! {
        done = output => done,
        done = input => done,
    }
}

/// Where the replay starts: the resume position while it is still valid, else recent output.
fn start(instance: &str, last_seq: u64, resume: Option<Resume>) -> u64 {
    let recent = recent_start(last_seq);
    match resume {
        Some(Resume {
            seq,
            instance: Some(at),
        }) if at == instance && (recent..=last_seq + 1).contains(&seq) => seq,
        _ => recent,
    }
}

async fn apply(typist: &Client, frame: ToHost) -> anyhow::Result<()> {
    match frame {
        ToHost::Input(data) => typist.write_bytes(&data).await.context("typing"),
        ToHost::Resize { cols, rows } => typist.resize(cols, rows).await.context("resizing"),
        ToHost::Unknown(_) => Ok(()),
    }
}

async fn send(out: &mut Stdout, frame: FromHost) -> anyhow::Result<()> {
    out.write_all(&frame.encode()).await?;
    out.flush().await?;
    Ok(())
}

/// The terminal's foreground program; `None` at the shell's prompt (or before the shell
/// reported its pid).
fn foreground(shell: &ShellTarget) -> Option<String> {
    let pid = shell.shell_pid()?;
    let group = proc::foreground_group(pid)?;
    if u32::try_from(group).ok() == Some(pid) {
        return None;
    }
    proc::name(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resume(seq: u64, instance: &str) -> Option<Resume> {
        Some(Resume {
            seq,
            instance: Some(instance.to_owned()),
        })
    }

    #[test]
    fn a_resume_continues_only_on_the_same_instance_within_the_window() {
        assert_eq!(start("a", 5000, None), 4000);
        assert_eq!(start("a", 10, None), 1);
        assert_eq!(start("a", 5000, resume(4500, "a")), 4500);
        assert_eq!(start("a", 5000, resume(5001, "a")), 5001, "nothing missed");
        assert_eq!(start("a", 5000, resume(4500, "b")), 4000, "reopened shell");
        assert_eq!(start("a", 5000, resume(10, "a")), 4000, "too far behind");
        assert_eq!(
            start("a", 5000, resume(6000, "a")),
            4000,
            "ahead of the log"
        );
        let anonymous = Some(Resume {
            seq: 4500,
            instance: None,
        });
        assert_eq!(start("a", 5000, anonymous), 4000);
    }
}

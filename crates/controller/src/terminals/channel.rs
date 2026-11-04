//! One attach: `controlroomd pty attach --framed` on an exec channel, pumped until the channel
//! ends, the browser falls behind or leaves, or the stream is closed.

use std::time::Duration;

use controlroom_client_core::bootstrap::daemon_command;
use controlroom_client_core::ssh::{ExecWriter, ExitStatus, SshConnection};
use controlroom_protocol::host::terminal::{Decoder, Frame as _, FromHost, MAX_PAYLOAD, ToHost};
use controlroom_protocol::ids::ShellId;
use controlroom_protocol::terminal::TerminalEvent;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, watch};

use super::{Input, TerminalFrame, closed};
use crate::hub::CloseReason;

/// How long a channel whose output ended may take to report its exit status.
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// What carries over from one attach to the next.
#[derive(Debug, Default)]
pub(super) struct Position {
    /// The shell instance the browser shows.
    pub instance: Option<String>,
    /// The next transcript position the browser needs.
    pub next_seq: u64,
    /// The browser's terminal size, sent again on every attach.
    pub size: Option<(u16, u16)>,
    /// The foreground program the browser was last told about (`None`: not told yet).
    pub foreground: Option<Option<String>>,
    /// How many attaches reached the browser.
    pub attaches: u64,
}

/// How an attach ended.
#[derive(Debug)]
pub(super) enum Outcome {
    /// The shell's process ended, or no such shell runs: end the stream.
    Ended(String),
    /// The channel or the link broke: attach again once the machine is reachable.
    Lost(String),
    /// The browser fell behind: attach again once it has caught up.
    Behind,
    /// The browser left, or the stream was closed.
    Closed,
}

/// The stream's queues.
pub(super) struct Io<'a> {
    pub input: &'a mut mpsc::Receiver<Input>,
    pub output: &'a mpsc::Sender<TerminalFrame>,
    pub close: &'a mut watch::Receiver<Option<CloseReason>>,
}

/// Why a frame couldn't be queued for the browser.
enum Undelivered {
    Behind,
    Gone,
}

impl From<Undelivered> for Outcome {
    fn from(why: Undelivered) -> Self {
        match why {
            Undelivered::Behind => Outcome::Behind,
            Undelivered::Gone => Outcome::Closed,
        }
    }
}

/// Queues a frame without waiting: the channel must keep being read.
fn deliver(output: &mpsc::Sender<TerminalFrame>, frame: TerminalFrame) -> Result<(), Undelivered> {
    output.try_send(frame).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => Undelivered::Behind,
        mpsc::error::TrySendError::Closed(_) => Undelivered::Gone,
    })
}

/// Attaches to `shell` over `ssh`, continuing at `pos`.
pub(super) async fn attach(
    ssh: &SshConnection,
    shell: &ShellId,
    pos: &mut Position,
    io: Io<'_>,
) -> Outcome {
    let seq = pos.next_seq.to_string();
    let mut args = vec!["pty", "attach", shell.as_str(), "--framed"];
    if let Some(instance) = &pos.instance {
        args.extend(["--since-seq", &seq, "--instance", instance]);
    }
    let channel = match ssh.exec(&daemon_command(&args)).await {
        Ok(channel) => channel,
        Err(e) => return Outcome::Lost(format!("opening the terminal channel: {e}")),
    };
    let (mut reader, mut writer, mut exit) = channel.into_parts();
    if let Some((cols, rows)) = pos.size
        && send(&mut writer, &ToHost::Resize { cols, rows })
            .await
            .is_err()
    {
        return Outcome::Lost("the terminal channel closed".to_owned());
    }

    let mut decoder = Decoder::default();
    let mut buf = vec![0; 64 * 1024];
    loop {
        tokio::select! {
            biased;
            () = closed(io.close) => return Outcome::Closed,
            read = reader.read(&mut buf) => {
                let n = match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                decoder.push(&buf[..n]);
                loop {
                    match decoder.next_frame::<FromHost>() {
                        Ok(Some(frame)) => {
                            if let Err(why) = receive(frame, pos, io.output) {
                                return why.into();
                            }
                        }
                        Ok(None) => break,
                        Err(e) => return Outcome::Lost(e.to_string()),
                    }
                }
            }
            input = io.input.recv() => {
                let frames = match input {
                    None => return Outcome::Closed,
                    Some(Input::Resize { cols, rows }) => {
                        pos.size = Some((cols, rows));
                        vec![ToHost::Resize { cols, rows }]
                    }
                    Some(Input::Bytes(bytes)) => bytes
                        .chunks(MAX_PAYLOAD)
                        .map(|chunk| ToHost::Input(chunk.to_vec()))
                        .collect(),
                };
                if send_all(&mut writer, &frames).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(reader);
    let status = tokio::time::timeout(EXIT_WAIT, exit.wait())
        .await
        .unwrap_or(ExitStatus::Unknown);
    match status {
        ExitStatus::Code(0) => Outcome::Ended("the shell's process ended".to_owned()),
        ExitStatus::Code(_) => {
            let stderr = String::from_utf8_lossy(&exit.stderr()).trim().to_owned();
            let reason = stderr.lines().last().unwrap_or("could not attach");
            Outcome::Ended(reason.to_owned())
        }
        _ if ssh.is_closed() => Outcome::Lost("the link dropped".to_owned()),
        _ => Outcome::Lost("the terminal channel closed".to_owned()),
    }
}

/// Handles one frame from the host: queues what the browser needs and advances `pos` only
/// for what was queued.
fn receive(
    frame: FromHost,
    pos: &mut Position,
    output: &mpsc::Sender<TerminalFrame>,
) -> Result<(), Undelivered> {
    match frame {
        FromHost::Attached(attached) => {
            let reset = pos.instance.as_deref() != Some(attached.instance.as_str())
                || pos.next_seq != attached.from_seq;
            deliver(
                output,
                TerminalFrame::Event(TerminalEvent::Attached { reset }),
            )?;
            pos.instance = Some(attached.instance);
            pos.next_seq = attached.from_seq;
            pos.attaches += 1;
        }
        FromHost::Output { seq, data } => {
            if seq >= pos.next_seq {
                deliver(output, TerminalFrame::Output(data))?;
                pos.next_seq = seq + 1;
            }
        }
        FromHost::Foreground(name) => {
            if pos.foreground.as_ref() != Some(&name) {
                let event = TerminalEvent::Foreground { name: name.clone() };
                deliver(output, TerminalFrame::Event(event))?;
                pos.foreground = Some(name);
            }
        }
        FromHost::Unknown(_) => {}
    }
    Ok(())
}

async fn send_all(writer: &mut ExecWriter, frames: &[ToHost]) -> std::io::Result<()> {
    for frame in frames {
        send(writer, frame).await?;
    }
    Ok(())
}

async fn send(writer: &mut ExecWriter, frame: &ToHost) -> std::io::Result<()> {
    writer.write_all(&frame.encode()).await?;
    writer.flush().await
}

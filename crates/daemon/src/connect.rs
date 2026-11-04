//! `controlroomd connect`: bridges stdin/stdout to the daemon socket, starting the daemon first
//! if none is running (spec §3). The controller runs it over an SSH exec channel.
//!
//! Bytes are copied unchanged in both directions. When stdin ends (the channel closed), the
//! socket's write half is shut down, and the bridge exits once the daemon closes its side.
//! Disconnecting does nothing else: the daemon, its shells and its jobs keep running. Errors go
//! to stderr only, so stdout stays a clean JSON-RPC stream.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::dirs::Dirs;
use crate::instance;

/// How long to keep retrying the socket after a start (e.g. a concurrent start that won the
/// lock and is still binding).
const CONNECT_RETRY: Duration = Duration::from_secs(10);

/// Arguments of `controlroomd connect`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let result = Dirs::from_env(args.state_dir.as_deref())
        .map_err(anyhow::Error::from)
        .and_then(|dirs| connect_or_start(&dirs))
        .and_then(bridge);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroomd connect: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Connects to the daemon socket, starting a detached daemon if nothing answers.
pub fn connect_or_start(dirs: &Dirs) -> anyhow::Result<UnixStream> {
    let socket = dirs.daemon_socket();
    if let Ok(stream) = UnixStream::connect(socket.as_path()) {
        return Ok(stream);
    }
    let exe = instance::serve_binary()?;
    instance::spawn_detached(&exe, dirs)?;
    let deadline = Instant::now() + CONNECT_RETRY;
    loop {
        match UnixStream::connect(socket.as_path()) {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() >= deadline => {
                return Err(e).with_context(|| format!("connecting to {socket}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn bridge(stream: UnixStream) -> anyhow::Result<()> {
    let mut to_daemon = stream.try_clone()?;
    let mut from_daemon = stream;
    std::thread::spawn(move || {
        let _ = pump(&mut std::io::stdin().lock(), &mut to_daemon);
        let _ = to_daemon.shutdown(Shutdown::Write);
    });
    pump(&mut from_daemon, &mut std::io::stdout().lock()).context("relaying to stdout")
}

/// Copies until EOF, flushing after every read. (Not `io::copy`, whose splice fast path can
/// hold data back between pipes and sockets.)
fn pump(from: &mut impl Read, to: &mut impl Write) -> std::io::Result<()> {
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n])?;
        to.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_copies_everything() {
        let input = b"{\"a\":1}\n{\"b\":2}\n".repeat(5000);
        let mut out = Vec::new();
        pump(&mut input.as_slice(), &mut out).unwrap();
        assert_eq!(out, input);
    }
}

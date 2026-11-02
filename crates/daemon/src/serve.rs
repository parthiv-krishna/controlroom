//! `controlroomd serve`: the per-user daemon.
//!
//! **PLACEHOLDER (W3c-2 replaces the server half).** The session runtime isn't wired in yet, so
//! this serve claims the instance (lock and socket, see [`crate::instance`]) and answers every
//! `hello` with `unsupported` (any other method with `hello_required`). Starting and detaching
//! are final: `connect` and `serve --detach` (e.g. from a `crontab @reboot` line) use them as
//! they are, and SIGTERM ends the daemon (default action).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use controlroom_protocol::daemon::{HelloParams, methods};
use controlroom_protocol::jsonrpc::encode_line;
use controlroom_protocol::{
    DAEMON_PROTOCOL_VERSION, ErrorData, Message, Method, RpcError, RpcResponse,
};
use serde_json::Value;

use crate::dirs::Dirs;
use crate::instance::{self, Claim, ReadyLine, Started};
use crate::version::VERSION;

/// Longest request line accepted.
const MAX_LINE_BYTES: u64 = 16 << 20;

/// Arguments of `controlroomd serve`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Start a detached daemon and exit once its socket is ready (exit 0 if one is running).
    #[arg(long, conflicts_with = "detached_child")]
    detach: bool,
    /// Internal: the detached daemon started by `--detach` or `connect`.
    /// (Its flag is [`instance::DETACHED_CHILD_FLAG`].)
    #[arg(long = "detached-child", hide = true)]
    detached_child: bool,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let result = Dirs::from_env(args.state_dir.as_deref())
        .map_err(anyhow::Error::from)
        .and_then(|dirs| {
            if args.detach {
                start_detached(&dirs)
            } else {
                serve(&dirs, args.detached_child)
            }
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if args.detached_child {
                ReadyLine::Error(format!("{e:#}")).report();
            }
            eprintln!("controlroomd serve: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn start_detached(dirs: &Dirs) -> anyhow::Result<()> {
    let exe = instance::serve_binary()?;
    match instance::spawn_detached(&exe, dirs)? {
        Started::New => eprintln!("controlroomd serve: started"),
        Started::AlreadyRunning => eprintln!("controlroomd serve: already running"),
    }
    Ok(())
}

fn serve(dirs: &Dirs, detached: bool) -> anyhow::Result<()> {
    if detached {
        instance::detach_from_launcher()?;
    }
    let instance = match instance::claim(dirs)? {
        Claim::Acquired(instance) => instance,
        Claim::AlreadyRunning => {
            if detached {
                ReadyLine::AlreadyRunning.report();
                return Ok(());
            }
            anyhow::bail!("another daemon is running ({})", dirs.daemon_lock());
        }
    };
    if detached {
        ReadyLine::Ready.report();
    }
    for stream in instance.listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(e) = serve_connection(stream) {
                        eprintln!("controlroomd serve: connection: {e}");
                    }
                });
            }
            Err(e) => eprintln!("controlroomd serve: accept: {e}"),
        }
    }
    Ok(())
}

fn serve_connection(stream: UnixStream) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.by_ref().take(MAX_LINE_BYTES).read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') && n as u64 >= MAX_LINE_BYTES {
            return Err(std::io::Error::other("request line too long"));
        }
        if let Some(response) = answer(&line) {
            writer.write_all(encode_line(&response)?.as_bytes())?;
        }
    }
}

/// The placeholder's answer to one line (`None` for notifications).
fn answer(line: &str) -> Option<RpcResponse<Value>> {
    let request = match Message::parse(line) {
        Ok(Message::Request(request)) => request,
        Ok(_) => return None,
        Err(e) => return Some(RpcResponse::error(e.id, *e.error)),
    };
    let error = if request.method == methods::Hello::NAME {
        hello_error(request.params)
    } else {
        RpcError::new(ErrorData::HelloRequired, "call hello first")
    };
    Some(RpcResponse::error(Some(request.id), error))
}

fn hello_error(params: Value) -> RpcError {
    match serde_json::from_value::<HelloParams>(params) {
        Err(e) => RpcError::new(ErrorData::InvalidParams, format!("invalid hello: {e}")),
        Ok(hello)
            if !hello
                .protocol_version
                .is_compatible_with(DAEMON_PROTOCOL_VERSION) =>
        {
            RpcError::new(
                ErrorData::IncompatibleVersion {
                    protocol_version: DAEMON_PROTOCOL_VERSION,
                    version: VERSION.to_owned(),
                },
                format!(
                    "protocol {} is incompatible with {DAEMON_PROTOCOL_VERSION}",
                    hello.protocol_version
                ),
            )
        }
        Ok(_) => RpcError::new(
            ErrorData::Unsupported,
            format!("controlroomd {VERSION} has no session runtime yet (placeholder serve)"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use controlroom_protocol::{RpcId, RpcOutcome};

    fn kind(response: &RpcResponse<Value>) -> Option<ErrorData> {
        match &response.outcome {
            RpcOutcome::Error(e) => e.data.clone(),
            RpcOutcome::Result(_) => None,
        }
    }

    #[test]
    fn placeholder_answers() {
        let hello = r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol_version":{"major":1,"minor":0},"client_name":"t","role":{"kind":"debug"}}}"#;
        let r = answer(hello).unwrap();
        assert_eq!(r.id, Some(RpcId::Number(1)));
        assert_eq!(kind(&r), Some(ErrorData::Unsupported));

        let old = hello.replace(r#""major":1"#, r#""major":0"#);
        assert!(matches!(
            kind(&answer(&old).unwrap()),
            Some(ErrorData::IncompatibleVersion { .. })
        ));
        let other = r#"{"jsonrpc":"2.0","id":"x","method":"agents.list","params":{}}"#;
        assert_eq!(
            kind(&answer(other).unwrap()),
            Some(ErrorData::HelloRequired)
        );
        assert!(answer(r#"{"jsonrpc":"2.0","method":"x"}"#).is_none());
        assert_eq!(
            kind(&answer("not json").unwrap()),
            Some(ErrorData::ParseError)
        );
    }
}

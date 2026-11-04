//! `controlroomd pty attach|resize <shell id>`: a user's terminal on a named shell (spec §22),
//! over its own SSH exec channel so bulk bytes never ride the host API.
//!
//! `attach` connects straight to the shell's PTY supervisor: it replays recent output (the
//! last [`REPLAY_RECORDS`] records, or from `--since-seq`), then streams the terminal's bytes
//! to stdout and types stdin into it, until the shell exits or stdin closes. Several attaches
//! (and the agent) share the terminal. `resize` sets the terminal size.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use controlroom_supervisor::Client;
use controlroom_transcript::PtyChunk;
use rusqlite::OptionalExtension;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::paths::HostPaths;
use super::store::Store;
use crate::dirs::Dirs;

/// Records replayed on attach by default.
const REPLAY_RECORDS: u64 = 1000;

/// `controlroomd pty …`.
#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Attach stdin/stdout to a shell's terminal.
    Attach {
        /// The shell (`<session id>/<name>`).
        shell: String,
        /// Replay output from this transcript position instead of the recent records.
        #[arg(long)]
        since_seq: Option<u64>,
    },
    /// Set a shell's terminal size.
    Resize {
        /// The shell (`<session id>/<name>`).
        shell: String,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
}

/// Runs `controlroomd pty …`.
pub fn run(args: Args) -> ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| rt.block_on(run_async(args)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroomd pty: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_async(args: Args) -> anyhow::Result<()> {
    let paths = HostPaths::new(Dirs::from_env(args.state_dir.as_deref())?);
    match args.command {
        Command::Attach { shell, since_seq } => {
            let client = connect(&paths, &shell).await?;
            attach(&client, &connect(&paths, &shell).await?, since_seq).await
        }
        Command::Resize { shell, cols, rows } => {
            let client = connect(&paths, &shell).await?;
            client.resize(cols, rows).await.context("resizing")
        }
    }
}

/// Connects to the running supervisor of `shell`.
async fn connect(paths: &HostPaths, shell: &str) -> anyhow::Result<Client> {
    let store = Store::open(paths.database().as_path())?;
    let id = shell.to_owned();
    let supervisor: Option<Option<String>> = store
        .run_blocking(|c| {
            c.query_row("SELECT supervisor FROM shells WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()
        })
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let Some(Some(supervisor)) = supervisor else {
        bail!("no running shell {shell:?} on this host");
    };
    let socket = paths.supervisor_socket(&supervisor);
    Client::connect(socket.as_path(), false)
        .await
        .with_context(|| format!("shell {shell:?} is not running"))
}

async fn attach(reader: &Client, typist: &Client, since_seq: Option<u64>) -> anyhow::Result<()> {
    let from = match since_seq {
        Some(seq) => seq,
        None => reader
            .status()
            .await?
            .last_seq
            .saturating_sub(REPLAY_RECORDS)
            .max(1),
    };
    let mut stream = reader.read(from).await?;
    let output = async {
        let mut stdout = tokio::io::stdout();
        while let Some(record) = stream.next().await {
            if let Some(chunk) = PtyChunk::from_record(&record) {
                stdout.write_all(&chunk.0).await?;
                stdout.flush().await?;
            }
        }
        anyhow::Ok(())
    };
    let input = async {
        let mut stdin = tokio::io::stdin();
        let mut buf = vec![0; 16 * 1024];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                return anyhow::Ok(());
            }
            typist.write_bytes(&buf[..n]).await?;
        }
    };
    tokio::select! {
        done = output => done,
        done = input => done,
    }
}

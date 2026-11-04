//! `controlroomd pty attach|resize <shell id>`: a user's terminal on a named shell (spec §22),
//! over its own SSH exec channel so bulk bytes never ride the host API.
//!
//! `attach` connects straight to the shell's PTY supervisor: it replays recent output (the
//! last [`REPLAY_RECORDS`] records, or from `--since-seq`), then streams the terminal's bytes
//! to stdout and types stdin into it, until the shell exits or stdin closes. Several attaches
//! (and the agent) share the terminal. `resize` sets the terminal size.
//!
//! Raw bytes suit a human at a terminal; the controller uses `--framed`
//! ([`controlroom_protocol::host::terminal`], implemented in [`framed`]): resize in-band,
//! transcript positions for resuming after a link drop, and the foreground program.

mod framed;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use controlroom_supervisor::Client;
use controlroom_transcript::PtyChunk;
use rusqlite::OptionalExtension;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        /// Speak the framed protocol (for the controller) instead of raw bytes.
        #[arg(long)]
        framed: bool,
        /// With `--framed`: `--since-seq` applies only while the shell still runs as this
        /// instance (from the `attached` frame); otherwise recent output is replayed.
        #[arg(long, requires_all = ["since_seq", "framed"])]
        instance: Option<String>,
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
        .and_then(|rt| {
            let result = rt.block_on(run_async(args));
            // A stdin read may still block a thread after the shell ended; don't wait for it.
            rt.shutdown_background();
            result
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroomd pty: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_async(args: Args) -> anyhow::Result<()> {
    let dirs = Dirs::from_env(args.state_dir.as_deref())?;
    match args.command {
        Command::Attach {
            shell,
            since_seq,
            framed: true,
            instance,
        } => {
            let shell = ShellTarget::find(&dirs, &shell)?;
            let resume = since_seq.map(|seq| framed::Resume { seq, instance });
            framed::attach(&shell, resume).await
        }
        Command::Attach {
            shell, since_seq, ..
        } => {
            let shell = ShellTarget::find(&dirs, &shell)?;
            attach(&shell.connect().await?, &shell.connect().await?, since_seq).await
        }
        Command::Resize { shell, cols, rows } => {
            let client = ShellTarget::find(&dirs, &shell)?.connect().await?;
            client.resize(cols, rows).await.context("resizing")
        }
    }
}

/// A running shell, as `host.sqlite` knows it.
struct ShellTarget {
    id: String,
    store: Store,
    socket: PathBuf,
}

impl ShellTarget {
    /// Finds the running shell `id`.
    fn find(dirs: &Dirs, id: &str) -> anyhow::Result<Self> {
        let store = Store::open(dirs.database().as_path())?;
        let key = id.to_owned();
        let supervisor: Option<Option<String>> = store
            .run_blocking(|c| {
                c.query_row("SELECT supervisor FROM shells WHERE id = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()
            })
            .map_err(|e| anyhow::anyhow!(e.message))?;
        let Some(Some(supervisor)) = supervisor else {
            bail!("no running shell {id:?} on this host");
        };
        Ok(Self {
            id: id.to_owned(),
            socket: dirs.supervisor_socket(&supervisor).as_path().to_owned(),
            store,
        })
    }

    /// Connects to the shell's supervisor.
    async fn connect(&self) -> anyhow::Result<Client> {
        Client::connect(&self.socket, false)
            .await
            .with_context(|| format!("shell {:?} is not running", self.id))
    }

    /// The shell's own pid, once its prompt reported it.
    fn shell_pid(&self) -> Option<u32> {
        let key = self.id.clone();
        self.store
            .run_blocking(|c| {
                c.query_row("SELECT shell_pid FROM shells WHERE id = ?1", [key], |r| {
                    r.get::<_, Option<u32>>(0)
                })
                .optional()
            })
            .ok()
            .flatten()
            .flatten()
    }
}

/// Where a replay starts by default: the last [`REPLAY_RECORDS`] records.
fn recent_start(last_seq: u64) -> u64 {
    last_seq.saturating_sub(REPLAY_RECORDS).max(1)
}

async fn attach(reader: &Client, typist: &Client, since_seq: Option<u64>) -> anyhow::Result<()> {
    let from = match since_seq {
        Some(seq) => seq,
        None => recent_start(reader.status().await?.last_seq),
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

//! `controlroomd credential <session> <provider>`: prints a provider's API key for nono's
//! `credential_capture` (spec §11.3).
//!
//! nono runs this host-side, unsandboxed, and injects stdout into requests on the provider's
//! credential route. The key goes to stdout only, without a trailing newline; errors go to
//! stderr and never contain key material.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use controlroom_sandbox::credential::{load_manifest, resolve_api_key};
use controlroom_sandbox::{AbsPath, Name, SessionId, SessionLayout};

/// Arguments of `controlroomd credential`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Daemon state directory (default: `~/.local/state/controlroom`).
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Session id (ULID).
    session: String,
    /// pi provider id.
    provider: String,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    match key(&args) {
        Ok(key) => {
            let mut out = std::io::stdout().lock();
            if out
                .write_all(key.expose().as_bytes())
                .and_then(|()| out.flush())
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("controlroomd credential: {e}");
            ExitCode::FAILURE
        }
    }
}

fn key(args: &Args) -> Result<controlroom_sandbox::credential::Secret, String> {
    let state_dir = match &args.state_dir {
        Some(dir) => AbsPath::from_path(dir).map_err(|e| e.to_string())?,
        None => crate::dirs::Dirs::from_env(None)
            .map_err(|e| e.to_string())?
            .state()
            .clone(),
    };
    let session = SessionId::new(&args.session).map_err(|e| e.to_string())?;
    let provider = Name::new(&args.provider).map_err(|e| e.to_string())?;
    let manifest =
        load_manifest(&SessionLayout::new(&state_dir, &session)).map_err(|e| e.to_string())?;
    resolve_api_key(&manifest, &provider).map_err(|e| e.to_string())
}

//! `controlroom-agentd credential <session> <provider>`: prints a provider's real credential for
//! nono's `credential_capture` (spec §11.3): its API key, or for an OAuth provider a fresh
//! access token from the controlroom-pi bundle's helper, refreshed by pi under the agent dir's
//! `auth.json` lock (the agent daemon is the only refresher of model credentials).
//!
//! nono runs this host-side, unsandboxed, and injects stdout into requests on the provider's
//! credential route. The credential goes to stdout only, without a trailing newline; errors go
//! to stderr and never contain key or token material.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use controlroom_sandbox::credential::{Secret, load_manifest, resolve_credential};
use controlroom_sandbox::{AbsPath, Name, SessionId, SessionLayout};

/// Arguments of `controlroom-agentd credential`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Agent daemon state directory [default: as for `serve`].
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// OAuth tokens must stay valid this long after they are printed (nono caches them for the
    /// route's TTL); refreshed first otherwise. API keys are unaffected.
    #[arg(long, default_value_t = 600)]
    valid_for_secs: u32,
    /// Session id (ULID).
    session: String,
    /// pi provider id.
    provider: String,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    match credential(&args) {
        Ok(secret) => {
            let mut out = std::io::stdout().lock();
            if out
                .write_all(secret.expose().as_bytes())
                .and_then(|()| out.flush())
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("controlroom-agentd credential: {e}");
            ExitCode::FAILURE
        }
    }
}

fn credential(args: &Args) -> Result<Secret, String> {
    let state_dir = match &args.state_dir {
        Some(dir) => AbsPath::from_path(dir).map_err(|e| e.to_string())?,
        None => crate::dirs::Dirs::from_env(crate::dirs::Overrides::default())
            .map_err(|e| e.to_string())?
            .state()
            .clone(),
    };
    let session = SessionId::new(&args.session).map_err(|e| e.to_string())?;
    let provider = Name::new(&args.provider).map_err(|e| e.to_string())?;
    let manifest =
        load_manifest(&SessionLayout::new(&state_dir, &session)).map_err(|e| e.to_string())?;
    resolve_credential(&manifest, &provider, args.valid_for_secs).map_err(|e| e.to_string())
}

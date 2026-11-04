//! OAuth access tokens, fresh from the controlroom-pi bundle's helper (`controlroom-pi-oauth`).
//!
//! The sandbox's `auth.json` only holds a synthetic token with a far-future expiry, so the
//! sandboxed pi never refreshes (and a refresh attempted inside the sandbox fails closed: the
//! provider's token endpoint is not on any route). controlroomd is the only refresher: the
//! helper runs pi's own locked refresh against the user's real `auth.json`, so it coordinates
//! with the user's pi processes, and prints the access token, valid for at least the requested
//! time (nono caches it for the route's TTL).

use std::process::Command;
use std::time::Duration;

use super::process::{self, RunError};
use super::{CredentialError, CredentialManifest, Secret};
use crate::names::Name;

/// The helper's own deadline is 50 s (pi waits up to 30 s for the lock and 15 s for the
/// refresh); this bounds a helper that hangs regardless.
const HELPER_TIMEOUT: Duration = Duration::from_secs(55);

/// Longest helper error message passed on.
const MAX_ERROR_LEN: usize = 500;

/// Gets `provider`'s access token through the manifest's OAuth helper, refreshed if it would
/// expire within `valid_for_secs`.
pub fn fresh_oauth_token(
    manifest: &CredentialManifest,
    provider: &Name,
    valid_for_secs: u32,
) -> Result<Secret, CredentialError> {
    let name = provider.as_str();
    if !manifest.oauth_providers.iter().any(|p| p == name) {
        return Err(CredentialError::UnknownProvider(name.to_owned()));
    }
    let helper = manifest
        .oauth_helper
        .as_ref()
        .ok_or_else(|| CredentialError::NoOAuthHelper(name.to_owned()))?;
    let mut command = Command::new(helper.as_path());
    command
        .arg("--agent-dir")
        .arg(manifest.agent_dir.as_str())
        .arg("--provider")
        .arg(name)
        .arg("--valid-for-secs")
        .arg(valid_for_secs.to_string())
        .env("PI_CODING_AGENT_DIR", manifest.agent_dir.as_str());
    let failed = |problem: String| CredentialError::OAuth {
        provider: name.to_owned(),
        problem,
    };
    let out = process::run(command, HELPER_TIMEOUT).map_err(|e| {
        failed(match e {
            RunError::Spawn(e) => format!("{helper}: {e}"),
            RunError::TimedOut => format!("{helper} timed out"),
        })
    })?;
    // The helper's messages never contain token material; stdout is the token.
    let token = out.stdout.trim();
    if !out.success || token.is_empty() {
        let message = out.stderr.trim();
        let message: String = message.chars().take(MAX_ERROR_LEN).collect();
        return Err(failed(if message.is_empty() {
            format!("{helper} printed no token")
        } else {
            message
        }));
    }
    Ok(Secret(token.to_owned()))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::paths::AbsPath;

    /// A helper script that records its arguments and runs `body`.
    fn manifest_with_helper(body: &str) -> (tempfile::TempDir, CredentialManifest) {
        let t = tempfile::tempdir().unwrap();
        let helper = t.path().join("helper");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" \"$PI_CODING_AGENT_DIR\" > {}/args\n{body}\n",
            t.path().display()
        );
        write_executable(&helper, &script);
        let manifest = CredentialManifest {
            agent_dir: AbsPath::new("/home/alice/.pi/agent").unwrap(),
            api_key_providers: vec!["gateway".into()],
            oauth_providers: vec!["sub".into()],
            oauth_helper: Some(AbsPath::from_path(&helper).unwrap()),
        };
        (t, manifest)
    }

    /// Writes an executable script from a child shell. Writing it from this process would race
    /// with concurrent tests: a child forked while the write fd is open inherits it until it
    /// execs, and executing the script then fails with ETXTBSY.
    fn write_executable(path: &std::path::Path, content: &str) {
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
            .arg("sh")
            .arg(content)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn name(s: &str) -> Name {
        Name::new(s).unwrap()
    }

    #[test]
    fn runs_the_helper_for_oauth_providers() {
        let (t, m) = manifest_with_helper("printf 'access-token\\n'");
        let token = fresh_oauth_token(&m, &name("sub"), 360).unwrap();
        assert_eq!(token.expose(), "access-token");
        let args = std::fs::read_to_string(t.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "--agent-dir",
                "/home/alice/.pi/agent",
                "--provider",
                "sub",
                "--valid-for-secs",
                "360",
                "/home/alice/.pi/agent"
            ]
        );
    }

    #[test]
    fn helper_failures_carry_its_message() {
        let (_t, m) =
            manifest_with_helper("echo 'refreshing sub failed: invalid_grant' >&2; exit 1");
        let err = fresh_oauth_token(&m, &name("sub"), 360).unwrap_err();
        assert!(
            err.to_string()
                .contains("refreshing sub failed: invalid_grant"),
            "{err}"
        );
        let (_t, m) = manifest_with_helper("exit 0");
        let err = fresh_oauth_token(&m, &name("sub"), 360).unwrap_err();
        assert!(err.to_string().contains("printed no token"), "{err}");
    }

    #[test]
    fn only_oauth_providers_with_a_helper() {
        let (_t, mut m) = manifest_with_helper("printf token");
        assert!(matches!(
            fresh_oauth_token(&m, &name("gateway"), 360),
            Err(CredentialError::UnknownProvider(_))
        ));
        m.oauth_helper = None;
        assert!(matches!(
            fresh_oauth_token(&m, &name("sub"), 360),
            Err(CredentialError::NoOAuthHelper(_))
        ));
    }
}

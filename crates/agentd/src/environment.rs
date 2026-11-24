//! The variables removed from the daemon's environment before anything else runs (see `main`),
//! so that no process it starts (supervisors, nono, pi, helpers) inherits them:
//!
//! - `NONO_*`: nono reads flags such as `NONO_ALLOW` from its environment, which would widen
//!   the sandbox;
//! - **provider credentials** ([`PROVIDER_VARS`]): pi takes an API key from the environment
//!   for every provider in its env-key table, so a key set for the service would reach every
//!   session whatever its profile admits (design `docs/design/profiles-tenants.md` §2, A7).
//!   Sessions get their credentials only through nono's credential routes.

use controlroom_sandbox::launch::NONO_ENV_PREFIX;

/// The variables pi 1.2.0 reads provider credentials from: its env-key table
/// (`@earendil-works/pi-ai` `env-api-keys.js`) plus the ambient credentials it accepts for
/// Vertex AI and Bedrock. Re-check on every pi bump.
pub const PROVIDER_VARS: &[&str] = &[
    "AI_GATEWAY_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANT_LING_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_PROFILE",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AZURE_OPENAI_API_KEY",
    "BASETEN_API_KEY",
    "CEREBRAS_API_KEY",
    "CLOUDFLARE_API_KEY",
    "COPILOT_GITHUB_TOKEN",
    "DEEPSEEK_API_KEY",
    "FIREWORKS_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_API_KEY",
    "GROQ_API_KEY",
    "HF_TOKEN",
    "KIMI_API_KEY",
    "META_API_KEY",
    "MINIMAX_API_KEY",
    "MINIMAX_CN_API_KEY",
    "MISTRAL_API_KEY",
    "MOONSHOT_API_KEY",
    "NVIDIA_API_KEY",
    "OPENAI_API_KEY",
    "OPENCODE_API_KEY",
    "OPENROUTER_API_KEY",
    "QWEN_TOKEN_PLAN_API_KEY",
    "QWEN_TOKEN_PLAN_CN_API_KEY",
    "RADIUS_API_KEY",
    "TOGETHER_API_KEY",
    "TYPESAFE_API_KEY",
    "XAI_API_KEY",
    "XIAOMI_API_KEY",
    "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
    "XIAOMI_TOKEN_PLAN_CN_API_KEY",
    "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
    "ZAI_API_KEY",
    "ZAI_CODING_CN_API_KEY",
];

/// Whether `key` is removed from the daemon's environment.
pub fn is_stripped(key: &str) -> bool {
    key.starts_with(NONO_ENV_PREFIX) || PROVIDER_VARS.contains(&key)
}

/// Removes every stripped variable. Call it first thing in `main`, before any other thread
/// exists.
pub fn strip_inherited() {
    let keys: Vec<_> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| k.to_str().is_some_and(is_stripped))
        .collect();
    for key in keys {
        // SAFETY: this runs first thing in `main`, before any other thread exists, so nothing
        // can read the environment concurrently.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nono_flags_and_provider_keys_are_stripped() {
        for key in [
            "NONO_ALLOW",
            "OPENAI_API_KEY",
            "ANTHROPIC_OAUTH_TOKEN",
            "HF_TOKEN",
        ] {
            assert!(is_stripped(key), "{key}");
        }
        for key in [
            "PATH",
            "HOME",
            "CREDENTIALS_DIRECTORY",
            "SHELL",
            "MY_API_KEY_FILE",
        ] {
            assert!(!is_stripped(key), "{key}");
        }
        let mut sorted = PROVIDER_VARS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, PROVIDER_VARS, "sorted, without duplicates");
    }
}

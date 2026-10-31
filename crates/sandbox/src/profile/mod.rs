//! nono profile generation and validation (spec §11.2).
//!
//! Profiles are absolute-path JSON files generated from a typed [`SessionSandboxSpec`]. They
//! never reference registry packs or `ns/name` profiles and never `extends` anything: nono
//! merges its built-in `default` groups implicitly.
//!
//! [`SessionSandboxSpec`]: crate::spec::SessionSandboxSpec

mod generate;
mod model;
mod validate;

pub use generate::generate_profile;
pub use model::NonoProfile;
pub use validate::{ProfileError, check_profile_json, validate_spec};

impl NonoProfile {
    /// Pretty-printed JSON with a trailing newline, as written to `profile.json`.
    pub fn to_json_pretty(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("profile serialization cannot fail");
        s.push('\n');
        s
    }

    /// The profile as a JSON value (for [`check_profile_json`]).
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("profile serialization cannot fail")
    }
}

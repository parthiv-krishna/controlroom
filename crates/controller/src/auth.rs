//! Who may use the controller: identities, the allow-list, and login sessions.
//!
//! The web shell authenticates (OIDC or the dev token) and hands the controller an
//! [`Identity`]; [`AuthPolicy::resolve`] admits it or not and maps it to an owner. A login
//! session ([`crate::Controller::create_login`]) stores the mode, subject and groups, so the
//! allow-list is re-checked on every request and a session made under another auth mode is
//! never accepted. v1 has one owner: every admitted identity maps to [`OwnerId::local`].

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::DeviceId;
use controlroom_protocol::ui::{AuthMode, UserInfo};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use sha2::{Digest, Sha256};

use crate::store::{AuthSessionRow, OwnerId, StoreError};

/// The subject of dev-token logins.
pub const DEV_SUBJECT: &str = "dev";

/// An authenticated principal, as established by the web shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// A verified OIDC login.
    Oidc {
        /// The `sub` claim.
        subject: String,
        /// The groups claim.
        groups: Vec<String>,
        /// The display name, if supplied.
        name: Option<String>,
        /// The email, if supplied.
        email: Option<String>,
    },
    /// The development token (loopback only).
    DevToken,
}

/// Who may sign in.
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    /// The deployment's auth mode; identities and sessions of another mode are refused.
    pub mode: AuthMode,
    /// OIDC subjects that may sign in.
    pub allowed_subjects: Vec<String>,
    /// OIDC groups whose members may sign in.
    pub allowed_groups: Vec<String>,
}

/// An admitted identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Whose data it may access.
    pub owner: OwnerId,
    /// How it signed in.
    pub mode: AuthMode,
    /// The OIDC subject or [`DEV_SUBJECT`].
    pub subject: String,
    /// The OIDC groups.
    pub groups: Vec<String>,
    /// Display name.
    pub name: Option<String>,
    /// Email.
    pub email: Option<String>,
}

impl Principal {
    /// The `hello` user.
    pub fn user_info(&self) -> UserInfo {
        UserInfo {
            id: self.subject.clone(),
            name: self.name.clone(),
            email: self.email.clone(),
        }
    }

    pub(crate) fn from_session(row: &AuthSessionRow) -> Self {
        Self {
            owner: row.owner.clone(),
            mode: row.mode,
            subject: row.subject.clone(),
            groups: row.groups.clone(),
            name: row.name.clone(),
            email: row.email.clone(),
        }
    }
}

/// Why a request isn't authenticated.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// No live login session for the token.
    #[error("not signed in")]
    NoSession,
    /// The identity or session uses another auth mode than the deployment.
    #[error("signed in with another authentication mode")]
    WrongMode,
    /// The allow-list doesn't admit the subject (anymore).
    #[error("{0} is not allowed to sign in")]
    NotAllowed(String),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl AuthPolicy {
    /// Whether the policy admits `subject` with `groups` under `mode`.
    pub fn admits(&self, mode: AuthMode, subject: &str, groups: &[String]) -> bool {
        if mode != self.mode {
            return false;
        }
        match mode {
            AuthMode::DevToken => subject == DEV_SUBJECT,
            AuthMode::Oidc => {
                self.allowed_subjects.iter().any(|s| s == subject)
                    || groups.iter().any(|g| self.allowed_groups.contains(g))
            }
            AuthMode::Unknown => false,
        }
    }

    /// Admits `identity` or says why not.
    pub fn resolve(&self, identity: &Identity) -> Result<Principal, AuthError> {
        let principal = match identity {
            Identity::Oidc {
                subject,
                groups,
                name,
                email,
            } => Principal {
                owner: OwnerId::local(),
                mode: AuthMode::Oidc,
                subject: subject.clone(),
                groups: groups.clone(),
                name: name.clone(),
                email: email.clone(),
            },
            Identity::DevToken => Principal {
                owner: OwnerId::local(),
                mode: AuthMode::DevToken,
                subject: DEV_SUBJECT.into(),
                groups: Vec::new(),
                name: None,
                email: None,
            },
        };
        self.check(&principal)?;
        Ok(principal)
    }

    /// Re-checks an admitted principal against the current policy.
    pub(crate) fn check(&self, p: &Principal) -> Result<(), AuthError> {
        if p.mode != self.mode {
            return Err(AuthError::WrongMode);
        }
        if !self.admits(p.mode, &p.subject, &p.groups) {
            return Err(AuthError::NotAllowed(p.subject.clone()));
        }
        Ok(())
    }
}

/// A new login session's secrets, for the cookie and CSRF header.
#[derive(Debug, Clone)]
pub struct NewLogin {
    /// The cookie value. Only its hash is stored.
    pub token: String,
    /// The CSRF token for non-WebSocket POSTs.
    pub csrf_token: String,
    /// When it expires unless used.
    pub expires_at: UnixMillis,
}

/// An authenticated request.
#[derive(Debug, Clone)]
pub struct Authenticated {
    /// Who.
    pub principal: Principal,
    /// The login session's token hash (identifies it for revocation).
    pub token_hash: String,
    /// The login session's CSRF token.
    pub csrf_token: String,
    /// The browser the session belongs to, once known.
    pub device_id: Option<DeviceId>,
}

/// A random URL-safe token with 256 bits of entropy.
pub(crate) fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random number generator works");
    BASE64URL_NOPAD.encode(&bytes)
}

/// The stored form of a cookie token.
pub fn token_hash(token: &str) -> String {
    HEXLOWER.encode(&Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: AuthMode) -> AuthPolicy {
        AuthPolicy {
            mode,
            allowed_subjects: vec!["alice".into()],
            allowed_groups: vec!["staff".into()],
        }
    }

    fn oidc(subject: &str, groups: &[&str]) -> Identity {
        Identity::Oidc {
            subject: subject.into(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
            name: None,
            email: None,
        }
    }

    #[test]
    fn the_allow_list_and_mode_decide() {
        let p = policy(AuthMode::Oidc);
        assert!(p.resolve(&oidc("alice", &[])).is_ok());
        assert!(p.resolve(&oidc("bob", &["staff"])).is_ok());
        assert!(matches!(
            p.resolve(&oidc("bob", &["guests"])),
            Err(AuthError::NotAllowed(_))
        ));
        assert!(matches!(
            p.resolve(&Identity::DevToken),
            Err(AuthError::WrongMode)
        ));
        let dev = policy(AuthMode::DevToken);
        assert_eq!(
            dev.resolve(&Identity::DevToken).unwrap().subject,
            DEV_SUBJECT
        );
        assert!(dev.resolve(&oidc("alice", &[])).is_err());
        // A principal admitted earlier fails once the list changes.
        let alice = p.resolve(&oidc("alice", &[])).unwrap();
        let mut narrowed = p.clone();
        narrowed.allowed_subjects.clear();
        assert!(narrowed.check(&alice).is_err());
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let (a, b) = (random_token(), random_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert_eq!(token_hash(&a).len(), 64);
        assert_ne!(token_hash(&a), token_hash(&b));
    }
}

//! The per-session directory layout owned by controlroomd (spec §11.1).
//!
//! ```text
//! <state>/sessions/<id>/agent/          PI_CODING_AGENT_DIR (sandbox read-write)
//! <state>/sessions/<id>/home/           HOME (sandbox read-write)
//! <state>/sessions/<id>/tmp/            TMPDIR (sandbox read-write)
//! <state>/sessions/<id>/profile.json    generated nono profile (not visible to the sandbox)
//! <state>/sessions/<id>/credentials.json  credential manifest (not visible to the sandbox)
//! ```

use crate::names::SessionId;
use crate::paths::AbsPath;

/// Paths of one session's directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLayout {
    id: SessionId,
    root: AbsPath,
}

impl SessionLayout {
    /// The layout of session `id` under the daemon state directory `state_dir`.
    pub fn new(state_dir: &AbsPath, id: &SessionId) -> Self {
        Self {
            id: id.clone(),
            root: state_dir.join("sessions").join(id.as_str()),
        }
    }

    /// The session id.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// `sessions/<id>`.
    pub fn root(&self) -> &AbsPath {
        &self.root
    }

    /// `PI_CODING_AGENT_DIR`.
    pub fn agent(&self) -> AbsPath {
        self.root.join("agent")
    }

    /// The sandboxed `HOME`.
    pub fn home(&self) -> AbsPath {
        self.root.join("home")
    }

    /// The sandboxed `TMPDIR`.
    pub fn tmp(&self) -> AbsPath {
        self.root.join("tmp")
    }

    /// The generated nono profile.
    pub fn profile(&self) -> AbsPath {
        self.root.join("profile.json")
    }

    /// The credential manifest read by `controlroomd credential`.
    pub fn credential_manifest(&self) -> AbsPath {
        self.root.join("credentials.json")
    }

    /// The directories the sandbox may read and write.
    pub fn sandbox_dirs(&self) -> [AbsPath; 3] {
        [self.agent(), self.home(), self.tmp()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths() {
        let id = SessionId::new("01HZY3K4M5N6P7Q8R9S0T1V2W3").unwrap();
        let l = SessionLayout::new(
            &AbsPath::new("/home/alice/.local/share/controlroom").unwrap(),
            &id,
        );
        assert_eq!(
            l.agent().as_str(),
            "/home/alice/.local/share/controlroom/sessions/01HZY3K4M5N6P7Q8R9S0T1V2W3/agent"
        );
        assert_eq!(l.profile().file_name(), Some("profile.json"));
        assert!(l.root().contains(&l.tmp()));
    }
}

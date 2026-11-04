//! The agent daemon's configuration files in its configuration dir.
//!
//! - [`agents`]: `agents.toml`, the agent registry;
//! - [`sandbox`]: `sandbox.toml`, the agents' sandbox inputs (nono, the pi agent dir, the
//!   keep-list, model credential routes).
//!
//! Both files are optional. A file that exists but can't be read or parsed is reported (by
//! `doctor`) and its defaults apply, so a typo never takes the daemon down.

pub mod agents;
pub mod sandbox;

use serde::de::DeserializeOwned;

use controlroom_sandbox::AbsPath;

/// A loaded configuration file.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded<T> {
    /// The file, when it exists.
    pub path: Option<AbsPath>,
    /// The parsed contents, or the defaults.
    pub value: T,
    /// Why the file could not be used; the defaults apply then.
    pub error: Option<String>,
}

/// Loads `path` as TOML, falling back to `T::default()` when it is missing or invalid.
pub fn load_toml<T: DeserializeOwned + Default>(path: &AbsPath) -> Loaded<T> {
    match std::fs::read_to_string(path.as_path()) {
        Ok(text) => match toml::from_str(&text) {
            Ok(value) => Loaded {
                path: Some(path.clone()),
                value,
                error: None,
            },
            Err(e) => Loaded {
                path: Some(path.clone()),
                value: T::default(),
                error: Some(format!("{path}: {}; using defaults", e.message())),
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded {
            path: None,
            value: T::default(),
            error: None,
        },
        Err(e) => Loaded {
            path: Some(path.clone()),
            value: T::default(),
            error: Some(format!("{path}: {e}; using defaults")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        #[serde(default)]
        name: String,
    }

    #[test]
    fn missing_invalid_and_valid_files() {
        let t = tempfile::tempdir().unwrap();
        let dir = AbsPath::from_path(t.path()).unwrap();

        let missing: Loaded<Sample> = load_toml(&dir.join("missing.toml"));
        assert_eq!((missing.path, missing.error), (None, None));

        std::fs::write(dir.join("bad.toml").as_path(), "name = [").unwrap();
        let bad: Loaded<Sample> = load_toml(&dir.join("bad.toml"));
        assert_eq!(bad.value, Sample::default());
        assert!(bad.error.unwrap().contains("using defaults"));

        std::fs::write(dir.join("good.toml").as_path(), "name = \"x\"\n").unwrap();
        let good: Loaded<Sample> = load_toml(&dir.join("good.toml"));
        assert_eq!(good.value.name, "x");
        assert!(good.error.is_none());
    }
}

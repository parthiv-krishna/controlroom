use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::names::SessionId;

const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

/// Every file under `root` with its contents, for before/after comparison.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.display().to_string(), fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn set_readonly(root: &Path, readonly: bool) {
    let mut stack = vec![root.to_owned()];
    while let Some(p) = stack.pop() {
        let is_dir = p.is_dir();
        if is_dir {
            stack.extend(fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        }
        let mode = match (is_dir, readonly) {
            (true, true) => 0o555,
            (false, true) => 0o444,
            (true, false) => 0o755,
            (false, false) => 0o644,
        };
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn read_json(path: AbsPath) -> Value {
    serde_json::from_slice(&fs::read(path.as_path()).unwrap()).unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    user: AbsPath,
    layout: SessionLayout,
    bundle: AbsPath,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = AbsPath::from_path(tmp.path()).unwrap();
    let user = root.join("user/.pi/agent");
    fs::create_dir_all(user.join("ext-config").as_path()).unwrap();
    fs::write(
        user.join("settings.json").as_path(),
        json!({
            "defaultProvider": "gateway",
            "packages": ["npm:kept", "npm:personal"],
            "httpProxy": "http://proxy.example.com:3128"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        user.join("auth.json").as_path(),
        json!({"gateway": {"type": "api_key", "key": "sk-real-secret"}}).to_string(),
    )
    .unwrap();
    fs::write(
        user.join("models.json").as_path(),
        json!({"providers": {"local": {"apiKey": "sk-models-secret", "baseUrl": "https://llm.example.com"}}})
            .to_string(),
    )
    .unwrap();
    fs::write(user.join("AGENTS.md").as_path(), "be nice\n").unwrap();
    fs::write(user.join("ext-config/config.json").as_path(), "{}").unwrap();
    let bundle = root.join("bundle");
    fs::create_dir_all(bundle.as_path()).unwrap();
    let layout = SessionLayout::new(&root.join("state"), &SessionId::new(SESSION).unwrap());
    Fixture {
        _tmp: tmp,
        user,
        layout,
        bundle,
    }
}

fn spec(f: &Fixture) -> AgentDirSpec {
    AgentDirSpec {
        user_agent_dir: Some(f.user.clone()),
        keep: vec!["npm:kept".into()],
        npm: Some(f.bundle.clone()),
        copy: vec!["AGENTS.md".into(), "ext-config".into(), "missing.md".into()],
        oauth_claims: BTreeMap::new(),
        tmp_seed: None,
        oauth_helper: None,
        managed: ManagedProviders::default(),
    }
}

#[test]
fn builds_the_session_tree_without_touching_the_user_dir() {
    let f = fixture();
    let before = snapshot(f.user.as_path());
    set_readonly(f.user.as_path(), true);
    let built = build_session_dirs(&f.layout, &spec(&f));
    set_readonly(f.user.as_path(), false);
    let built = built.unwrap();
    assert_eq!(snapshot(f.user.as_path()), before);

    assert_eq!(built.api_key_providers, ["gateway", "local"]);
    let agent = f.layout.agent();
    assert_eq!(
        read_json(agent.join("settings.json")),
        json!({"defaultProvider": "gateway", "packages": ["npm:kept"], "transport": "sse"})
    );
    assert_eq!(
        read_json(agent.join("auth.json")),
        json!({"gateway": {"type": "api_key", "key": PLACEHOLDER}})
    );
    assert_eq!(
        read_json(agent.join("models.json"))["providers"]["local"]["apiKey"],
        PLACEHOLDER
    );
    assert_eq!(
        fs::read_link(agent.join("npm").as_path()).unwrap(),
        f.bundle.as_path()
    );
    assert_eq!(
        fs::read_to_string(agent.join("AGENTS.md").as_path()).unwrap(),
        "be nice\n"
    );
    assert!(agent.join("ext-config/config.json").as_path().is_file());
    assert!(agent.join("sessions").as_path().is_dir());
    assert!(f.layout.home().as_path().is_dir());
    assert!(f.layout.tmp().as_path().is_dir());

    // No secret reaches anything the sandbox can read.
    for (path, bytes) in snapshot(f.layout.root().as_path()) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("sk-real") && !text.contains("sk-models"),
            "{path}"
        );
    }
    let mode = fs::metadata(agent.join("auth.json").as_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let root_mode = fs::metadata(f.layout.root().as_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(root_mode & 0o777, 0o700);

    let manifest = crate::credential::load_manifest(&f.layout).unwrap();
    assert_eq!(manifest.agent_dir, f.user);
    assert_eq!(manifest.api_key_providers, ["gateway", "local"]);
}

#[test]
fn rebuild_regenerates_files_and_keeps_sessions() {
    let f = fixture();
    build_session_dirs(&f.layout, &spec(&f)).unwrap();
    let pi_session = f.layout.agent().join("sessions/--w--/s.jsonl");
    fs::create_dir_all(pi_session.parent().unwrap().as_path()).unwrap();
    fs::write(pi_session.as_path(), "{}\n").unwrap();
    fs::write(f.layout.home().join(".pi-lens").as_path(), "x").unwrap();
    // A sandboxed process tampered with a generated file.
    fs::write(f.layout.agent().join("auth.json").as_path(), "{}").unwrap();

    build_session_dirs(&f.layout, &spec(&f)).unwrap();
    assert!(pi_session.as_path().is_file());
    assert!(f.layout.home().join(".pi-lens").as_path().is_file());
    assert_eq!(
        read_json(f.layout.agent().join("auth.json"))["gateway"]["key"],
        PLACEHOLDER
    );
}

#[test]
fn tmp_seed_does_not_overwrite() {
    let f = fixture();
    let seed = f.bundle.join("seed");
    fs::create_dir_all(seed.join("jiti").as_path()).unwrap();
    fs::write(seed.join("jiti/a.js").as_path(), "seeded").unwrap();
    fs::write(seed.join("jiti/b.js").as_path(), "seeded").unwrap();
    fs::create_dir_all(f.layout.tmp().join("jiti").as_path()).unwrap();
    fs::write(f.layout.tmp().join("jiti/b.js").as_path(), "session").unwrap();
    let spec = AgentDirSpec {
        tmp_seed: Some(seed),
        ..spec(&f)
    };
    build_session_dirs(&f.layout, &spec).unwrap();
    let read = |n: &str| fs::read_to_string(f.layout.tmp().join(n).as_path()).unwrap();
    assert_eq!(read("jiti/a.js"), "seeded");
    assert_eq!(read("jiti/b.js"), "session");
}

#[test]
fn rejects_escaping_copy_entries() {
    let f = fixture();
    for bad in [
        "../x",
        "/etc/passwd",
        "auth.json",
        "sessions/x",
        "npm",
        "a/./b",
        "",
    ] {
        let spec = AgentDirSpec {
            copy: vec![bad.into()],
            ..spec(&f)
        };
        assert!(
            matches!(
                build_session_dirs(&f.layout, &spec),
                Err(AgentDirError::InvalidCopy(_))
            ),
            "{bad}"
        );
    }
}

#[test]
fn works_without_a_user_agent_dir() {
    let f = fixture();
    let built = build_session_dirs(&f.layout, &AgentDirSpec::default()).unwrap();
    assert!(built.api_key_providers.is_empty());
    assert_eq!(
        read_json(f.layout.agent().join("settings.json")),
        json!({"transport": "sse"})
    );
    assert!(!f.layout.credential_manifest().as_path().exists());
}

#[test]
fn merges_managed_providers_and_the_user_config_wins() {
    let f = fixture();
    let store = f.bundle.join("keys.json");
    let spec = AgentDirSpec {
        managed: ManagedProviders {
            // `gateway` has a key in the user's auth.json: theirs wins.
            key_providers: vec!["anthropic".into(), "gateway".into(), "ui-gw".into()],
            key_store: Some(store.clone()),
            models: serde_json::from_value(json!({
                "ui-gw": {"baseUrl": "https://gw.example.com/v1", "api": "openai-completions",
                          "apiKey": PLACEHOLDER, "models": [{"id": "m1"}]},
                "local": {"baseUrl": "https://shadowed.example.com"}
            }))
            .unwrap(),
            settings: serde_json::from_value(
                json!({"defaultProvider": "ui-gw", "defaultModel": "m1"}),
            )
            .unwrap(),
        },
        ..spec(&f)
    };
    let built = build_session_dirs(&f.layout, &spec).unwrap();
    assert_eq!(built.stored_key_providers, ["anthropic", "ui-gw"]);
    let agent = f.layout.agent();
    let auth = read_json(agent.join("auth.json"));
    assert_eq!(
        auth["anthropic"],
        json!({"type": "api_key", "key": PLACEHOLDER})
    );
    assert_eq!(auth["gateway"]["key"], PLACEHOLDER);
    // The gateway's placeholder is in its models.json entry.
    assert!(auth.get("ui-gw").is_none());
    let models = read_json(agent.join("models.json"));
    assert_eq!(
        models["providers"]["ui-gw"]["models"],
        json!([{"id": "m1"}])
    );
    assert_eq!(
        models["providers"]["local"]["baseUrl"],
        "https://llm.example.com"
    );
    let settings = read_json(agent.join("settings.json"));
    assert_eq!(settings["defaultProvider"], "ui-gw");
    assert_eq!(settings["defaultModel"], "m1");

    let manifest = crate::credential::load_manifest(&f.layout).unwrap();
    assert_eq!(
        manifest.stored_keys,
        Some(crate::credential::StoredKeys {
            path: store,
            providers: vec!["anthropic".into(), "ui-gw".into()],
        })
    );
}

#[test]
fn agent_config_alone() {
    let f = fixture();
    let dir = f.bundle.join("catalog");
    let built = build_agent_config(&dir, &spec(&f)).unwrap();
    assert_eq!(built.api_key_providers, ["gateway", "local"]);
    assert_eq!(
        read_json(dir.join("auth.json"))["gateway"]["key"],
        PLACEHOLDER
    );
    assert!(!dir.join("sessions").as_path().exists());
}

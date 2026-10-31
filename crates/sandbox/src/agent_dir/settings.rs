//! The session `settings.json`: the user's settings filtered to the keep-list.

use serde_json::{Map, Value};

/// Settings keys that list resources (extensions, packages, skills...). Only entries whose
/// source is on the keep-list survive.
const RESOURCE_KEYS: [&str; 5] = ["packages", "extensions", "skills", "prompts", "themes"];

/// Settings that must not carry over: a proxy would bypass nono's, and the session store
/// must stay inside the session agent dir.
const DROPPED_KEYS: [&str; 2] = ["httpProxy", "sessionDir"];

/// Builds the session settings from the user's: resource lists filtered to `keep`,
/// [`DROPPED_KEYS`] removed, and `"transport": "sse"` (nono's credential routes have no
/// WebSocket upgrade rule, so pi's WebSocket attempt would only fail first).
pub fn session_settings(user: &Map<String, Value>, keep: &[String]) -> Map<String, Value> {
    let mut out = user.clone();
    for key in DROPPED_KEYS {
        out.remove(key);
    }
    for key in RESOURCE_KEYS {
        if let Some(Value::Array(entries)) = out.get_mut(key) {
            entries.retain(|e| resource_source(e).is_some_and(|s| keep.iter().any(|k| k == s)));
        }
    }
    out.insert("transport".to_owned(), Value::String("sse".to_owned()));
    out
}

/// The source of a resource entry: the string itself or the object's `source`.
fn resource_source(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(s) => Some(s),
        Value::Object(o) => o.get("source").and_then(Value::as_str),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_resources_and_forces_sse() {
        let user = json!({
            "defaultProvider": "gateway",
            "transport": "websocket",
            "httpProxy": "http://proxy.example.com:3128",
            "sessionDir": "/home/alice/pi-sessions",
            "packages": [
                "npm:kept-extension",
                {"source": "/nix/store/aaaa-provider", "extensions": ["x"]},
                "npm:personal-extension",
                42
            ],
            "skills": ["/home/alice/skills"]
        });
        let keep = vec![
            "npm:kept-extension".to_owned(),
            "/nix/store/aaaa-provider".to_owned(),
        ];
        let out = session_settings(user.as_object().unwrap(), &keep);
        assert_eq!(
            Value::Object(out),
            json!({
                "defaultProvider": "gateway",
                "transport": "sse",
                "packages": [
                    "npm:kept-extension",
                    {"source": "/nix/store/aaaa-provider", "extensions": ["x"]}
                ],
                "skills": []
            })
        );
    }
}

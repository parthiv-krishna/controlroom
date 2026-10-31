//! The session `models.json`: the user's with every key and secret header replaced.

use serde_json::Value;

use super::PLACEHOLDER;

/// Replaces, anywhere in the document, every `apiKey` value and every secret-looking header
/// value with [`PLACEHOLDER`]. A header is secret if its name suggests a credential or its
/// value is resolved at runtime (`!command` or `$ENV`). The real values are injected by
/// nono's proxy, never seen by the sandbox.
pub fn sanitize_models(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "apiKey" && v.is_string() {
                    *v = Value::String(PLACEHOLDER.to_owned());
                } else if key == "headers" && v.is_object() {
                    sanitize_headers(v);
                } else {
                    sanitize_models(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(sanitize_models),
        _ => {}
    }
}

fn sanitize_headers(headers: &mut Value) {
    let Value::Object(map) = headers else { return };
    for (name, v) in map.iter_mut() {
        let Value::String(s) = v else { continue };
        if is_secret_header(name) || s.starts_with('!') || s.contains('$') {
            *v = Value::String(PLACEHOLDER.to_owned());
        }
    }
}

fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "authorization"
        || n == "cookie"
        || ["key", "token", "secret", "auth", "password"]
            .iter()
            .any(|w| n.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_keys_and_secret_headers() {
        let mut models = json!({
            "providers": {
                "local": {
                    "baseUrl": "https://llm.example.com/v1",
                    "apiKey": "sk-literal-secret",
                    "headers": {
                        "X-Api-Key": "literal",
                        "X-Team": "blue",
                        "X-Dynamic": "!pass show team",
                        "X-From-Env": "$TEAM_HEADER"
                    },
                    "models": [{"id": "m1", "headers": {"Authorization": "Bearer abc"}}]
                },
                "envkey": {"apiKey": "$GATEWAY_KEY"}
            }
        });
        sanitize_models(&mut models);
        assert_eq!(
            models,
            json!({
                "providers": {
                    "local": {
                        "baseUrl": "https://llm.example.com/v1",
                        "apiKey": PLACEHOLDER,
                        "headers": {
                            "X-Api-Key": PLACEHOLDER,
                            "X-Team": "blue",
                            "X-Dynamic": PLACEHOLDER,
                            "X-From-Env": PLACEHOLDER
                        },
                        "models": [{"id": "m1", "headers": {"Authorization": PLACEHOLDER}}]
                    },
                    "envkey": {"apiKey": PLACEHOLDER}
                }
            })
        );
    }
}

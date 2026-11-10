//! Provider tests: one cheap authenticated `GET` (e.g. the model list) to the provider's
//! upstream, with the credential in the header its route uses. The result names the HTTP
//! status or the connection error, never a response body (some APIs echo part of the key) and
//! never the key.

use std::time::Duration;

/// How long a test may take.
const TIMEOUT: Duration = Duration::from_secs(15);

/// One test request.
#[derive(Debug, Clone)]
pub struct ProbeRequest {
    /// The full URL.
    pub url: String,
    /// The credential's header.
    pub header: String,
    /// Its value template (`{}` is the credential).
    pub format: String,
    /// Extra headers the API requires.
    pub extra: Vec<(String, String)>,
    /// What a 401 means for this credential, when it says more than the status (e.g. that a
    /// subscription token expired or was revoked).
    pub unauthorized: Option<&'static str>,
}

/// Sends `request` with `secret`; `Ok` with a short description when the provider accepted it.
pub async fn probe(request: &ProbeRequest, secret: &str) -> Result<String, String> {
    let redact = |text: String| text.replace(secret, "<key>");
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut builder = client
        .get(&request.url)
        .header(&request.header, request.format.replace("{}", secret));
    for (name, value) in &request.extra {
        builder = builder.header(name, value);
    }
    let response = builder.send().await.map_err(|e| redact(describe(&e)))?;
    let status = response.status();
    let text = match (status.canonical_reason(), request.unauthorized) {
        (_, Some(meaning)) if status == reqwest::StatusCode::UNAUTHORIZED => {
            format!("HTTP 401: {meaning}")
        }
        (Some(reason), _) => format!("HTTP {} {reason}", status.as_u16()),
        (None, _) => format!("HTTP {}", status.as_u16()),
    };
    if status.is_success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// A reqwest error and its causes, as one line.
fn describe(error: &reqwest::Error) -> String {
    let mut text = if error.is_timeout() {
        "timed out".to_owned()
    } else if error.is_connect() {
        "can't connect".to_owned()
    } else {
        "request failed".to_owned()
    };
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text = format!("{text}: {cause_text}");
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::test_support::one_request_server;

    /// The test server, accepting `x-api-key: good-key`; its model list URL.
    async fn server() -> (String, tokio::task::JoinHandle<String>) {
        let (origin, task) = one_request_server("x-api-key: good-key").await;
        (format!("{origin}/v1/models"), task)
    }

    fn request(url: String) -> ProbeRequest {
        ProbeRequest {
            url,
            header: "x-api-key".into(),
            format: "{}".into(),
            extra: vec![("anthropic-version".into(), "2023-06-01".into())],
            unauthorized: None,
        }
    }

    #[tokio::test]
    async fn ok_and_rejected_without_the_body_or_key() {
        let (url, task) = server().await;
        assert_eq!(
            probe(&request(url), "good-key").await.unwrap(),
            "HTTP 200 OK"
        );
        assert!(
            task.await
                .unwrap()
                .contains("anthropic-version: 2023-06-01")
        );

        let (url, _task) = server().await;
        let err = probe(&request(url), "bad-key").await.unwrap_err();
        assert_eq!(err, "HTTP 401 Unauthorized");

        let err = probe(&request("http://127.0.0.1:1/v1/models".into()), "k")
            .await
            .unwrap_err();
        assert!(err.starts_with("can't connect"), "{err}");
    }
}

//! A small in-process OIDC provider: discovery, JWKS, authorize (approves the configured user
//! at once), token (checks client authentication, the redirect URI and PKCE; issues an RS256
//! ID token) and userinfo.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use data_encoding::{BASE64, BASE64URL_NOPAD};
use openidconnect::core::{
    CoreGenderClaim, CoreJsonWebKeySet, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm,
    CoreRsaPrivateSigningKey,
};
use openidconnect::{
    AccessToken, AdditionalClaims, Audience, IdToken, IdTokenClaims, IssuerUrl, JsonWebKeyId,
    Nonce, PrivateSigningKey, StandardClaims, SubjectIdentifier,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Digest;

use super::mock_idp_key::RSA_PEM;

/// Who the provider signs in.
#[derive(Debug, Clone)]
pub struct MockUser {
    pub subject: String,
    pub groups: Vec<String>,
    /// Put the groups into the ID token (else only into userinfo).
    pub groups_in_id_token: bool,
}

pub fn user(subject: &str, groups: &[&str], groups_in_id_token: bool) -> MockUser {
    MockUser {
        subject: subject.into(),
        groups: groups.iter().map(|g| (*g).into()).collect(),
        groups_in_id_token,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Groups {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    groups: Option<Vec<String>>,
}

impl AdditionalClaims for Groups {}

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    user: MockUser,
}

struct Idp {
    issuer: String,
    client_id: String,
    client_secret: String,
    key: CoreRsaPrivateSigningKey,
    user: Mutex<MockUser>,
    grants: Mutex<HashMap<String, Grant>>,
    access_tokens: Mutex<HashMap<String, MockUser>>,
    counter: AtomicU64,
}

impl Idp {
    fn unique(&self, prefix: &str) -> String {
        format!("{prefix}-{}", self.counter.fetch_add(1, Ordering::SeqCst))
    }
}

/// A running provider.
pub struct MockIdp {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    state: Arc<Idp>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockIdp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockIdp {
    pub async fn start(user: MockUser) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Idp {
            issuer: issuer.clone(),
            client_id: "controlroom".into(),
            client_secret: "client-secret-for-tests".into(),
            key: CoreRsaPrivateSigningKey::from_pem(RSA_PEM, Some(JsonWebKeyId::new("k1".into())))
                .unwrap(),
            user: Mutex::new(user),
            grants: Mutex::default(),
            access_tokens: Mutex::default(),
            counter: AtomicU64::new(1),
        });
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .route("/userinfo", get(userinfo))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            issuer,
            client_id: state.client_id.clone(),
            client_secret: state.client_secret.clone(),
            state,
            task,
        }
    }

    pub fn set_user(&self, user: MockUser) {
        *self.state.user.lock().unwrap() = user;
    }
}

async fn discovery(State(s): State<Arc<Idp>>) -> Json<serde_json::Value> {
    let i = &s.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "userinfo_endpoint": format!("{i}/userinfo"),
        "jwks_uri": format!("{i}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn jwks(State(s): State<Arc<Idp>>) -> Json<CoreJsonWebKeySet> {
    Json(CoreJsonWebKeySet::new(vec![s.key.as_verification_key()]))
}

fn bad(error: &str, description: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

async fn authorize(
    State(s): State<Arc<Idp>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    if get("client_id") != s.client_id || get("response_type") != "code" {
        return bad("invalid_request", "client_id or response_type");
    }
    if get("code_challenge_method") != "S256" || get("code_challenge").is_empty() {
        return bad("invalid_request", "PKCE S256 is required");
    }
    if !get("scope").split(' ').any(|x| x == "openid") || get("nonce").is_empty() {
        return bad(
            "invalid_request",
            "the openid scope and a nonce are required",
        );
    }
    let code = s.unique("code");
    s.grants.lock().unwrap().insert(
        code.clone(),
        Grant {
            nonce: get("nonce"),
            challenge: get("code_challenge"),
            redirect_uri: get("redirect_uri"),
            user: s.user.lock().unwrap().clone(),
        },
    );
    let mut url = url::Url::parse(&get("redirect_uri")).unwrap();
    url.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &get("state"));
    (StatusCode::FOUND, [(header::LOCATION, url.to_string())]).into_response()
}

async fn token(
    State(s): State<Arc<Idp>>,
    headers: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let basic = format!(
        "Basic {}",
        BASE64.encode(format!("{}:{}", s.client_id, s.client_secret).as_bytes())
    );
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if auth != Some(basic.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    if get("grant_type") != "authorization_code" {
        return bad("unsupported_grant_type", "authorization_code only");
    }
    let Some(grant) = s.grants.lock().unwrap().remove(&get("code")) else {
        return bad("invalid_grant", "unknown or used code");
    };
    if get("redirect_uri") != grant.redirect_uri {
        return bad("invalid_grant", "redirect_uri mismatch");
    }
    let challenge = BASE64URL_NOPAD.encode(&sha2::Sha256::digest(get("code_verifier").as_bytes()));
    if challenge != grant.challenge {
        return bad("invalid_grant", "PKCE verification failed");
    }
    let access_token = AccessToken::new(s.unique("access"));
    s.access_tokens
        .lock()
        .unwrap()
        .insert(access_token.secret().clone(), grant.user.clone());
    let now = chrono::Utc::now();
    let claims = IdTokenClaims::<Groups, CoreGenderClaim>::new(
        IssuerUrl::new(s.issuer.clone()).unwrap(),
        vec![Audience::new(s.client_id.clone())],
        now + chrono::Duration::seconds(300),
        now,
        StandardClaims::new(SubjectIdentifier::new(grant.user.subject.clone())),
        Groups {
            groups: grant
                .user
                .groups_in_id_token
                .then(|| grant.user.groups.clone()),
        },
    )
    .set_nonce(Some(Nonce::new(grant.nonce)));
    let id_token = IdToken::<
        Groups,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
    >::new(
        claims,
        &s.key,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        Some(&access_token),
        None,
    )
    .unwrap();
    Json(json!({
        "access_token": access_token.secret(),
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token.to_string(),
    }))
    .into_response()
}

async fn userinfo(State(s): State<Arc<Idp>>, headers: HeaderMap) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    match s.access_tokens.lock().unwrap().get(token) {
        Some(user) => Json(json!({"sub": user.subject, "groups": user.groups})).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

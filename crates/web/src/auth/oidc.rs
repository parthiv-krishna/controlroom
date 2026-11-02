//! OIDC: the authorization code flow with PKCE as a confidential client, configured by
//! discovery from the issuer.
//!
//! `GET /auth/login` seals `{state, nonce, PKCE verifier, return_to}` into the short-lived
//! login cookie ([`super::login_state`]) and redirects to the identity provider.
//! `GET /auth/callback` opens the cookie, checks `state`, exchanges the code with the
//! verifier, validates the ID token (signature, issuer, audience, expiry, nonce, `at_hash`),
//! reads the groups (ID token, else userinfo), and hands the identity to the controller, which
//! applies the allow-list now and on every later request.
//!
//! Outbound requests (discovery, token, userinfo) go through a client that never follows
//! redirects and a global [`RateLimiter`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use controlroom_controller::Identity;
use controlroom_protocol::UnixMillis;
use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType,
    CoreGenderClaim, CoreJsonWebKey, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm,
    CoreProviderMetadata, CoreRevocableToken, CoreRevocationErrorResponse,
    CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AccessToken, AccessTokenHash, AdditionalClaims, AuthorizationCode, Client, ClientId,
    ClientSecret, CsrfToken, EmptyExtraTokenFields, EndpointMaybeSet, EndpointNotSet, EndpointSet,
    IdTokenFields, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, StandardErrorResponse, StandardTokenResponse, SubjectIdentifier,
    TokenResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::cookies::read_cookie;
use super::limiter::RateLimiter;
use super::login_state::{LOGIN_TTL, LoginSealer, LoginState};
use super::{AuthRuntime, login_response, sanitize_return_to};
use crate::config::OidcSettings;
use crate::secrets::constant_time_eq;
use crate::server::AppState;

/// Re-run discovery (and refetch the keys) after this long.
const METADATA_TTL: Duration = Duration::from_secs(60 * 60);
/// Outbound identity-provider work unauthenticated clients may cause: a burst, then one
/// operation per interval.
const LIMIT_BURST: u32 = 30;
const LIMIT_INTERVAL: Duration = Duration::from_secs(2);
/// Timeout of each request to the identity provider.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// The `groups` claim: a list, a single string, or absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupsClaim {
    #[serde(
        default,
        deserialize_with = "groups_claim",
        skip_serializing_if = "Option::is_none"
    )]
    groups: Option<Vec<String>>,
}

impl AdditionalClaims for GroupsClaim {}

fn groups_claim<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(
        Option::<OneOrMany>::deserialize(d)?.map(|groups| match groups {
            OneOrMany::One(group) => vec![group],
            OneOrMany::Many(groups) => groups,
        }),
    )
}

type IdFields = IdTokenFields<
    GroupsClaim,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;
type Tokens = StandardTokenResponse<IdFields, CoreTokenType>;
type OidcClient = Client<
    GroupsClaim,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    Tokens,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// Why a login failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The login cookie is missing, forged or expired.
    #[error("the login expired or was started in another browser; please retry")]
    UnknownLogin,
    /// The callback's `state` doesn't match.
    #[error("state mismatch")]
    StateMismatch,
    /// The identity provider answered with an error.
    #[error("the identity provider returned an error: {0}")]
    Provider(String),
    /// Talking to the identity provider failed, or its answer is invalid.
    #[error("{0}")]
    Exchange(String),
    /// Too many logins at once.
    #[error("too many sign-in attempts; try again in a minute")]
    RateLimited,
    /// A configuration problem.
    #[error("internal error: {0}")]
    Internal(String),
}

impl LoginError {
    fn status(&self) -> StatusCode {
        match self {
            Self::UnknownLogin | Self::StateMismatch | Self::Provider(_) => StatusCode::BAD_REQUEST,
            Self::Exchange(_) => StatusCode::BAD_GATEWAY,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for LoginError {
    fn into_response(self) -> Response {
        tracing::warn!(error = %self, "OIDC sign-in failed");
        (
            self.status(),
            [(header::CACHE_CONTROL, "no-store")],
            format!("sign-in failed: {self}\n"),
        )
            .into_response()
    }
}

/// The OIDC relying party.
pub struct Oidc {
    settings: OidcSettings,
    client_secret: ClientSecret,
    redirect_url: RedirectUrl,
    http: reqwest::Client,
    metadata: RwLock<Option<(Instant, CoreProviderMetadata)>>,
    sealer: LoginSealer,
    limiter: RateLimiter,
}

impl Oidc {
    /// A relying party for `settings`, redirected back to `<public_origin>/auth/callback`.
    pub fn new(
        settings: OidcSettings,
        client_secret: String,
        public_origin: &str,
    ) -> Result<Self, LoginError> {
        let redirect_url = RedirectUrl::new(format!("{public_origin}/auth/callback"))
            .map_err(|e| LoginError::Internal(format!("redirect URL: {e}")))?;
        let http = reqwest::Client::builder()
            // Following redirects from the identity provider invites SSRF.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| LoginError::Internal(format!("HTTP client: {e}")))?;
        Ok(Self {
            settings,
            client_secret: ClientSecret::new(client_secret),
            redirect_url,
            http,
            metadata: RwLock::new(None),
            sealer: LoginSealer::default(),
            limiter: RateLimiter::new(LIMIT_BURST, LIMIT_INTERVAL),
        })
    }

    /// The discovered client. Discovery is cached; failures aren't, so an identity provider
    /// that is down at boot doesn't block later logins.
    async fn client(&self) -> Result<OidcClient, LoginError> {
        let cached = self
            .metadata
            .read()
            .await
            .as_ref()
            .filter(|(at, _)| at.elapsed() < METADATA_TTL)
            .map(|(_, md)| md.clone());
        let metadata = match cached {
            Some(md) => md,
            None => {
                let issuer = IssuerUrl::new(self.settings.issuer.clone())
                    .map_err(|e| LoginError::Internal(format!("issuer: {e}")))?;
                let md = CoreProviderMetadata::discover_async(issuer, &self.http)
                    .await
                    .map_err(|e| LoginError::Exchange(format!("OIDC discovery failed: {e}")))?;
                *self.metadata.write().await = Some((Instant::now(), md.clone()));
                md
            }
        };
        Ok(OidcClient::from_provider_metadata(
            metadata,
            ClientId::new(self.settings.client_id.clone()),
            Some(self.client_secret.clone()),
        )
        .set_redirect_uri(self.redirect_url.clone()))
    }

    /// Starts a login: the sealed login cookie value and the authorization URL.
    pub async fn start(
        &self,
        cookie_name: &str,
        return_to: String,
    ) -> Result<(String, String), LoginError> {
        if !self.limiter.try_acquire() {
            return Err(LoginError::RateLimited);
        }
        let client = self.client().await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        for scope in &self.settings.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, state, nonce) = request.url();
        let login = LoginState {
            state: state.secret().clone(),
            nonce: nonce.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
            return_to,
            expires_at: expiry(),
        };
        Ok((self.sealer.seal(cookie_name, &login), url.to_string()))
    }

    /// Finishes a login from the callback: the verified identity and where to return.
    pub async fn finish(
        &self,
        cookie_name: &str,
        cookie: Option<&str>,
        query: CallbackQuery,
    ) -> Result<(Identity, String), LoginError> {
        let login = cookie
            .and_then(|value| self.sealer.open(cookie_name, value, UnixMillis::now()))
            .ok_or(LoginError::UnknownLogin)?;
        if !query
            .state
            .as_deref()
            .is_some_and(|s| constant_time_eq(s, &login.state))
        {
            return Err(LoginError::StateMismatch);
        }
        if let Some(error) = query.error {
            let detail = query.error_description.unwrap_or_default();
            return Err(LoginError::Provider(format!("{error} {detail}")));
        }
        let code = query
            .code
            .ok_or_else(|| LoginError::Provider("no code".into()))?;
        if !self.limiter.try_acquire() {
            return Err(LoginError::RateLimited);
        }
        let client = self.client().await?;
        let tokens = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|e| LoginError::Internal(e.to_string()))?
            .set_pkce_verifier(PkceCodeVerifier::new(login.pkce_verifier))
            .request_async(&self.http)
            .await
            .map_err(|e| LoginError::Exchange(format!("token exchange failed: {e}")))?;
        let id_token = tokens
            .id_token()
            .ok_or_else(|| LoginError::Exchange("no ID token in the token response".into()))?;
        let verifier = client.id_token_verifier();
        let claims = id_token
            .claims(&verifier, &Nonce::new(login.nonce))
            .map_err(|e| LoginError::Exchange(format!("invalid ID token: {e}")))?;
        if let Some(expected) = claims.access_token_hash() {
            let invalid = |e: String| LoginError::Exchange(format!("at_hash: {e}"));
            let alg = id_token.signing_alg().map_err(|e| invalid(e.to_string()))?;
            let key = id_token
                .signing_key(&verifier)
                .map_err(|e| invalid(e.to_string()))?;
            let actual = AccessTokenHash::from_token(tokens.access_token(), alg, key)
                .map_err(|e| invalid(e.to_string()))?;
            if actual != *expected {
                return Err(invalid("mismatch".into()));
            }
        }
        let mut groups = claims.additional_claims().groups.clone();
        if groups.is_none() && !self.settings.allowed_groups.is_empty() {
            groups = self
                .userinfo_groups(&client, tokens.access_token().clone(), claims.subject())
                .await;
        }
        let name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.to_string())
            .or_else(|| claims.preferred_username().map(|n| n.to_string()));
        let identity = Identity::Oidc {
            subject: claims.subject().to_string(),
            groups: groups.unwrap_or_default(),
            name,
            email: claims.email().map(|e| e.to_string()),
        };
        Ok((identity, login.return_to))
    }

    async fn userinfo_groups(
        &self,
        client: &OidcClient,
        access_token: AccessToken,
        subject: &SubjectIdentifier,
    ) -> Option<Vec<String>> {
        let request = client.user_info(access_token, Some(subject.clone())).ok()?;
        match request
            .request_async::<GroupsClaim, _, CoreGenderClaim>(&self.http)
            .await
        {
            Ok(info) => info.additional_claims().groups.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "OIDC userinfo request failed");
                None
            }
        }
    }
}

fn expiry() -> UnixMillis {
    UnixMillis(UnixMillis::now().0 + LOGIN_TTL.as_millis() as i64)
}

/// `/auth/login` query.
#[derive(Deserialize)]
pub(crate) struct LoginQuery {
    return_to: Option<String>,
}

/// `/auth/callback` query.
#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn oidc(state: &AppState) -> Option<&Oidc> {
    match &state.auth {
        AuthRuntime::Oidc(oidc) => Some(oidc),
        AuthRuntime::DevToken(_) => None,
    }
}

/// `GET /auth/login?return_to=/path`.
pub(crate) async fn login(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LoginQuery>,
) -> Response {
    let Some(oidc) = oidc(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let name = state.cookies.login_name();
    match oidc
        .start(name, sanitize_return_to(query.return_to.as_deref()))
        .await
    {
        Ok((cookie, url)) => (
            StatusCode::SEE_OTHER,
            [
                (header::LOCATION, url),
                (header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            [(
                header::SET_COOKIE,
                state.cookies.set(name, &cookie, LOGIN_TTL),
            )],
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /auth/callback?code=…&state=…`.
pub(crate) async fn callback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(oidc) = oidc(&state) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let name = state.cookies.login_name();
    let cookie = read_cookie(&headers, name);
    let mut res = match oidc.finish(name, cookie.as_deref(), query).await {
        Ok((identity, return_to)) => login_response(&state, &identity, &return_to),
        Err(e) => e.into_response(),
    };
    res.headers_mut()
        .append(header::SET_COOKIE, state.cookies.clear(name));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_claim_shapes() {
        let parse = |json: &str| serde_json::from_str::<GroupsClaim>(json).unwrap().groups;
        assert_eq!(
            parse(r#"{"groups": ["a", "b"]}"#),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(parse(r#"{"groups": "a"}"#), Some(vec!["a".into()]));
        assert_eq!(parse(r#"{"groups": null}"#), None);
        assert_eq!(parse(r#"{"sub": "x"}"#), None);
    }
}

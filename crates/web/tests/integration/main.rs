//! controlroom-web integration tests over real listeners on 127.0.0.1, with an in-process
//! mock identity provider. No network access is needed.

mod common;
mod mock_idp;
mod mock_idp_key;

mod dev_token;
mod hardening;
mod oidc;
mod push;
mod startup;
mod static_files;
mod ws;

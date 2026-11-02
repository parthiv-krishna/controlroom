//! controlroom-web: the web server binary.

use std::process::ExitCode;

use clap::Parser;
use controlroom_web::{Cli, Config, Env, StartOptions, start};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroom-web: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let config = match &cli.config {
        Some(path) => Config::load(path)?,
        None => Config::default(),
    };
    let settings = config.with_cli(&cli).resolve(&Env::from_process())?;
    let server = start(settings, StartOptions::default()).await?;
    if let Some(url) = server.dev_login_url() {
        // Printed once, Jupyter-style; the token is also in <state_dir>/dev-token (0600).
        println!("controlroom-web is running in dev-token mode. Open:\n    {url}");
    }
    shutdown_signal().await;
    tracing::info!("shutting down");
    server.shutdown().await
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing the SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

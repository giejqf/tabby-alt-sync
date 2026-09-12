use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::Router;
use clap::Parser as _;
use tracing_subscriber::EnvFilter;

use tabby_alt_sync::api::{self, AppState};
use tabby_alt_sync::auth::TokenSecret;
use tabby_alt_sync::config::{self, Cli, Command, Settings};
use tabby_alt_sync::db::Db;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(Command::GenToken) = cli.command {
        println!("{}", config::generate_token());
        return Ok(());
    }

    let settings = Settings::from_cli(&cli).context("invalid configuration")?;
    init_tracing(&settings.log)?;

    let db = Db::open(&settings.db)
        .with_context(|| format!("failed to open database at {}", settings.db.display()))?;

    let state = AppState {
        db,
        secret: Arc::new(TokenSecret::new(&settings.token)),
        token: Arc::from(settings.token.as_str()),
        username: Arc::from(settings.username.as_str()),
        max_body_bytes: settings.max_body_bytes,
    };
    let app = api::router(state);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;

    runtime.block_on(serve(app, &settings))
}

fn init_tracing(filter: &str) -> Result<()> {
    let env_filter = EnvFilter::try_new(filter).context("invalid TABBY_ALT_SYNC_LOG filter")?;
    tracing_subscriber::fmt().with_env_filter(env_filter).init();
    Ok(())
}

async fn serve(app: Router, settings: &Settings) -> Result<()> {
    let handle = axum_server::Handle::new();
    let make_service = app.into_make_service();

    let mut server = match settings.tls.as_ref() {
        Some(tls) => {
            let rustls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert, &tls.key)
                .await
                .with_context(|| {
                    format!(
                        "failed to load TLS certificate {} / key {}",
                        tls.cert.display(),
                        tls.key.display()
                    )
                })?;
            tokio::spawn(
                axum_server::bind_rustls(settings.bind, rustls)
                    .handle(handle.clone())
                    .serve(make_service),
            )
        }
        None => {
            tracing::warn!("TLS is disabled; current Tabby clients refuse to sync over http://");
            tokio::spawn(
                axum_server::bind(settings.bind)
                    .handle(handle.clone())
                    .serve(make_service),
            )
        }
    };

    tracing::info!(
        bind = %settings.bind,
        tls = settings.tls.is_some(),
        max_body_bytes = settings.max_body_bytes,
        "listening"
    );

    tokio::select! {
        result = &mut server => {
            result
                .context("server task panicked")?
                .context("server stopped")?;
            return Ok(());
        }
        _ = shutdown_signal() => {}
    }

    tracing::info!("shutdown signal received");
    handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
    server
        .await
        .context("server task panicked")?
        .context("server stopped")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signals) => {
                signals.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

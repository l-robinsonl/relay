//! Relay's synchronisation server.
//!
//! Configured entirely from the environment; see `.env.example`. The
//! minimum to run it is nothing at all — it defaults to SQLite in
//! `data/relay.db` on `0.0.0.0:8787`.

use std::sync::Arc;

use relay_server::config::Config;
use relay_server::store::SyncStore;
use relay_server::{AppState, PostgresStore, SqliteStore, router};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RELAY_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn")),
        )
        .init();

    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Startup failures are read by whoever deployed this, so they
            // print the actual problem rather than "internal error".
            eprintln!("relay-server failed to start: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;

    let store: Arc<dyn SyncStore> = if config.is_postgres() {
        tracing::info!("connecting to PostgreSQL");
        Arc::new(
            PostgresStore::connect(&config.database_url)
                .await?
                .charging(config.bills()),
        )
    } else {
        let path = config.database_url.trim_start_matches("sqlite://");
        // sqlx will create the file but not its parent directory.
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        tracing::info!(%path, "opening SQLite database");
        Arc::new(
            SqliteStore::connect(&config.database_url)
                .await?
                .charging(config.bills()),
        )
    };

    store.migrate().await?;
    tracing::info!(backend = store.backend(), "schema is up to date");

    let bind = config.bind;
    let signup = format!("{:?}", config.signup_mode).to_lowercase();
    let state = AppState::new(store, config);

    if let Some(admin) = state.bootstrap_admin().await? {
        tracing::warn!(
            email = %admin.email,
            "bootstrap administrator created — remove RELAY_ADMIN_EMAIL and \
             RELAY_ADMIN_PASSWORD from the environment now that it exists"
        );
    }

    warn_about_the_configuration(&state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    let actual = listener.local_addr()?;
    tracing::info!(%actual, signup = %signup, "relay-server listening");

    // With connect info, so the rate limiter can tell callers apart.
    // Behind a proxy every request arrives from the proxy, which is
    // what RELAY_TRUST_FORWARDED_FOR is for.
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    tracing::info!("relay-server stopped");
    Ok(())
}

/// Says so at startup when the configuration will not do what it looks
/// like it does.
///
/// Each of these is survivable, so none of them refuses to start — a
/// sync server that will not come up is worse than one with an awkward
/// setting. They are logged at startup because that is the one moment
/// somebody is looking.
fn warn_about_the_configuration(state: &AppState) {
    let config = &state.config;

    if config.require_verified_email && !state.mailer.delivers() {
        tracing::warn!(
            "RELAY_REQUIRE_VERIFIED_EMAIL is on but no mail provider is \
             configured, so confirmation links are only written to this log. \
             Nobody but the bootstrap administrator will be able to sign in \
             until you set RESEND_API_KEY and RELAY_MAIL_FROM, or retrieve \
             each link from here by hand."
        );
    }

    // Every e-mailed link is built from this. A wrong value is invisible
    // until somebody cannot click an invitation.
    let public = config.public_url.trim_end_matches('/');
    if public.contains("0.0.0.0") || public.contains("127.0.0.1") || public.contains("localhost") {
        tracing::warn!(
            %public,
            "RELAY_PUBLIC_URL points at this machine, so e-mailed links will \
             not work for anybody else. Set it to the address people reach \
             this server on."
        );
    } else if public.starts_with("http://") {
        tracing::warn!(
            %public,
            "RELAY_PUBLIC_URL is http, so session tokens and invitation \
             links cross the network in the clear. Put this behind TLS."
        );
    }

    if config.signup_mode == relay_server::config::SignupMode::Open && !state.mailer.delivers() {
        tracing::warn!(
            "RELAY_SIGNUP_MODE is open and addresses are not confirmed, so \
             anyone who can reach this server can create an account. Use \
             `invite` unless that is what you want."
        );
    }
}

/// Resolves when the process is asked to stop.
///
/// Graceful shutdown matters here: a push interrupted mid-transaction
/// would be retried by the client anyway, but finishing in-flight requests
/// avoids a pointless round of conflict resolution on every deploy.
async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("could not listen for Ctrl-C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("could not listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => tracing::info!("received Ctrl-C"),
        () = terminate => tracing::info!("received SIGTERM"),
    }
}

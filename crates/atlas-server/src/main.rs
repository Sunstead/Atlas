//! `atlas`: the Sunstead Atlas server. One binary serves the `/v1` API, the
//! sign-in routes and the web app build.

mod api;
mod auth;
mod config;
mod error;
mod sources;
mod state;
mod web;

use atlas_state::{Db, MasterKey};
use auth::{Auth, Mode};
use axum::Router;
use config::{AuthMode, Config};
use state::AppState;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

/// Used only with `ATLAS_DEV_USER` and no `ATLAS_MASTER_KEY`, so local
/// development can save connections. Never protects anything real: the dev
/// mode that allows it signs anyone in.
const DEV_MASTER_KEY: [u8; 32] = *b"atlas-dev-key-not-for-real-data!";

#[tokio::main]
async fn main() {
    // Development convenience only: release builds take their config from
    // the real environment (compose), never a stray file.
    #[cfg(debug_assertions)]
    let dotenv = dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        // Colour only on a terminal; `docker logs` should be plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    #[cfg(debug_assertions)]
    if let Some(path) = dotenv {
        tracing::info!(path = %path.display(), "loaded development settings");
    }

    let config = match Config::from_env(|k| std::env::var(k).ok()) {
        Ok(c) => c,
        Err(e) => fail(2, &e.to_string()),
    };
    let state = match build_state(&config) {
        Ok(s) => s,
        Err(e) => fail(2, &e),
    };

    let listener = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(l) => l,
        Err(e) => fail(1, &format!("can't listen on {}: {e}", config.bind)),
    };
    tracing::info!(bind = %config.bind, public_url = %config.public_url, version = env!("CARGO_PKG_VERSION"), "atlas listening");

    spawn_purge(state.db.clone());
    if let Mode::Oidc(client) = &state.auth.mode {
        client.spawn_warmup();
    }

    let app = app(state, config.web_dir.as_deref());
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown()).await {
        fail(1, &format!("server error: {e}"));
    }
}

fn fail(code: i32, message: &str) -> ! {
    tracing::error!("{message}");
    std::process::exit(code);
}

fn build_state(config: &Config) -> Result<AppState, String> {
    let db = Db::open(&config.state_dir).map_err(|e| format!("state database in {}: {e}", config.state_dir.display()))?;

    let master_key = match (&config.master_key, &config.auth) {
        (Some(text), _) => Some(Arc::new(MasterKey::from_base64(text).map_err(|e| e.to_string())?)),
        (None, AuthMode::Dev { .. }) => {
            tracing::warn!("ATLAS_MASTER_KEY isn't set; using the built-in development key");
            Some(Arc::new(MasterKey::from_bytes(&DEV_MASTER_KEY).expect("32 bytes")))
        }
        (None, _) => {
            tracing::warn!("ATLAS_MASTER_KEY isn't set; connections that need a credential can't be saved");
            None
        }
    };

    match &config.auth {
        AuthMode::Oidc(o) => tracing::info!(issuer = %o.issuer, "sign-in with OIDC"),
        AuthMode::Dev { username } => {
            tracing::warn!(user = %username, "DEVELOPMENT SIGN-IN: anyone who reaches this server is signed in as this user")
        }
        AuthMode::Disabled => tracing::warn!("no sign-in configured (ATLAS_OIDC_ISSUER or ATLAS_DEV_USER); nobody can sign in"),
    }

    Ok(AppState {
        db,
        auth: Arc::new(Auth::new(config.auth.clone(), &config.public_url)),
        master_key,
        sources: Arc::new(config.sources.clone()),
        public_url: config.public_url.clone(),
    })
}

/// The whole HTTP surface. `web_dir` adds the app shell.
pub(crate) fn app(state: AppState, web_dir: Option<&Path>) -> Router {
    let mut app = api::router(state.clone()).merge(auth::router(state.clone()));
    if let Some(web) = web_dir.and_then(web::router) {
        app = app.merge(web);
    }
    // Paths only: queries carry search terms (`/search?q=`), which don't
    // belong in logs.
    app.layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
        tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
    }))
    .with_state(state)
}

/// Clears expired sessions and abandoned sign-ins every hour.
fn spawn_purge(db: Db) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            tick.tick().await;
            match db.purge_expired().await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(removed = n, "purged expired sessions"),
                Err(e) => tracing::warn!(error = %e, "can't purge expired sessions"),
            }
        }
    });
}

/// Ctrl-C, or SIGTERM from `docker stop`.
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}

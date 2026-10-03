//! `atlas`: the Sunstead Atlas server. One binary serves the `/v1` API, the
//! sign-in routes and the web app build.

mod api;
mod config;
mod error;
mod web;

use axum::Router;
use config::Config;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        // Colour only on a terminal; `docker logs` should be plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let config = match Config::from_env(|k| std::env::var(k).ok()) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(2);
        }
    };

    let listener = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(bind = %config.bind, "can't listen: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(bind = %config.bind, version = env!("CARGO_PKG_VERSION"), "atlas listening");

    if let Err(e) = axum::serve(listener, app(&config)).with_graceful_shutdown(shutdown()).await {
        tracing::error!("server error: {e}");
        std::process::exit(1);
    }
}

fn app(config: &Config) -> Router {
    let mut app = api::router();
    if let Some(web) = config.web_dir.as_deref().and_then(web::router) {
        app = app.merge(web);
    }
    // Paths only: queries carry search terms (`/search?q=`), which don't
    // belong in logs.
    app.layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
        tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
    }))
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

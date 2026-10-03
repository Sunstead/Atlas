//! `/auth/login`, `/auth/callback` and `/auth/logout`.

use super::oidc::OidcError;
use super::page::message;
use super::return_to::safe_return_to;
use super::{session_user, Mode};
use crate::api::csrf;
use crate::error::AppError;
use crate::state::AppState;
use atlas_common::LogoutResponse;
use atlas_state::{random_token, NewUser, OidcFlow, SESSION_TTL_SECS};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout).layer(middleware::from_fn_with_state(state, csrf::guard)))
}

/// A redirect that's never cached: it depends on the session.
fn redirect(to: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to), (header::CACHE_CONTROL, "no-store")]).into_response()
}

/// Signs `user` in: a session, its cookie, and on to `return_to`.
async fn start_session(state: &AppState, user: NewUser, return_to: &str) -> Response {
    let username = user.username.clone();
    let result = async {
        let user = state.db.upsert_user(user).await?;
        state.db.create_session(user.id).await
    }
    .await;
    match result {
        Ok(token) => {
            tracing::info!(user = %username, "signed in");
            let mut res = redirect(return_to);
            res.headers_mut().insert(header::SET_COOKIE, state.auth.cookie.set(&token, SESSION_TTL_SECS));
            res
        }
        Err(e) => {
            tracing::error!(error = %e, "can't start a session");
            message(StatusCode::INTERNAL_SERVER_ERROR, "Couldn't sign you in", "Something went wrong on the server. Try again.", Some(("/", "Try again")))
        }
    }
}

#[derive(Deserialize)]
struct LoginQuery {
    return_to: Option<String>,
}

async fn login(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<LoginQuery>) -> Response {
    let return_to = safe_return_to(&state.public_url, q.return_to.as_deref());
    if let Ok(Some(_)) = session_user(&state, &headers).await {
        return redirect(&return_to);
    }

    match &state.auth.mode {
        Mode::Disabled => message(
            StatusCode::NOT_IMPLEMENTED,
            "Sign-in isn't set up",
            "This server has no identity provider configured. Set ATLAS_OIDC_ISSUER, or ATLAS_DEV_USER for local development.",
            None,
        ),
        Mode::Dev { username } => {
            let user = NewUser {
                issuer: "dev".into(),
                subject: username.clone(),
                username: username.clone(),
                display_name: Some(username.clone()),
                email: None,
            };
            start_session(&state, user, &return_to).await
        }
        Mode::Oidc(client) => {
            let flow = OidcFlow {
                state: random_token(32),
                pkce_verifier: random_token(32),
                nonce: random_token(32),
                return_to,
            };
            let url = client.authorize_url(&state.auth.redirect_uri, &flow.state, &flow.nonce, &flow.pkce_verifier).await;
            match url {
                Ok(url) => match state.db.put_flow(flow).await {
                    Ok(()) => redirect(&url),
                    Err(e) => {
                        tracing::error!(error = %e, "can't store the sign-in flow");
                        message(StatusCode::INTERNAL_SERVER_ERROR, "Couldn't start sign-in", "Something went wrong on the server. Try again.", Some(("/", "Try again")))
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "can't reach the identity provider");
                    unavailable()
                }
            }
        }
    }
}

fn unavailable() -> Response {
    message(
        StatusCode::SERVICE_UNAVAILABLE,
        "Can't reach sign-in",
        "The identity provider isn't answering right now. Try again in a moment.",
        Some(("/", "Try again")),
    )
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(State(state): State<AppState>, Query(q): Query<CallbackQuery>) -> Response {
    let Mode::Oidc(client) = &state.auth.mode else {
        return redirect("/");
    };
    // Taken first, so a refused or broken sign-in can't be replayed either.
    let flow = match q.state.as_deref() {
        Some(s) => state.db.take_flow(s).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "can't read the sign-in flow");
            None
        }),
        None => None,
    };
    let Some(flow) = flow else {
        return message(
            StatusCode::BAD_REQUEST,
            "This sign-in link has expired",
            "Sign-in links work once, for ten minutes. Start again.",
            Some(("/auth/login", "Sign in")),
        );
    };
    let retry = format!("/auth/login?return_to={}", url::form_urlencoded::byte_serialize(flow.return_to.as_bytes()).collect::<String>());

    if let Some(error) = q.error {
        tracing::info!(%error, description = q.error_description.as_deref().unwrap_or(""), "the provider refused sign-in");
        let body = q.error_description.unwrap_or_else(|| "The identity provider refused the sign-in.".into());
        return message(StatusCode::FORBIDDEN, "Sign-in didn't complete", &body, Some((&retry, "Try again")));
    }
    let Some(code) = q.code else {
        return message(StatusCode::BAD_REQUEST, "Sign-in didn't complete", "The provider sent no code.", Some((&retry, "Try again")));
    };

    let claims = match client.exchange(&state.auth.redirect_uri, &code, &flow.pkce_verifier, &flow.nonce).await {
        Ok(c) => c,
        Err(OidcError::Unavailable(e)) => {
            tracing::warn!(error = %e, "can't finish sign-in");
            return unavailable();
        }
        Err(OidcError::Invalid(e)) => {
            tracing::warn!(error = %e, "sign-in refused");
            return message(StatusCode::BAD_REQUEST, "Sign-in didn't complete", "The sign-in couldn't be verified. Start again.", Some((&retry, "Try again")));
        }
    };

    let allowed = &client.config().allowed_groups;
    if !allowed.is_empty() && !claims.groups.iter().any(|g| allowed.contains(g)) {
        tracing::info!(sub = %claims.sub, "signed in but not in an allowed group");
        return message(
            StatusCode::FORBIDDEN,
            "No access to Atlas",
            "Your account isn't in a group that can use Atlas. Ask the server's admin.",
            None,
        );
    }

    let username = claims.preferred_username.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| claims.sub.clone());
    let user = NewUser {
        issuer: client.config().issuer.clone(),
        subject: claims.sub,
        username,
        display_name: claims.name.filter(|n| !n.is_empty()),
        email: claims.email.filter(|e| !e.is_empty()),
    };
    start_session(&state, user, &flow.return_to).await
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    if let Some(token) = state.auth.cookie.read(&headers) {
        state.db.delete_session(&token).await?;
    }
    let redirect = match &state.auth.mode {
        Mode::Oidc(client) => client.end_session_url().await.unwrap_or_else(|| "/".into()),
        _ => "/".into(),
    };
    let mut res = Json(LogoutResponse { redirect }).into_response();
    res.headers_mut().insert(header::SET_COOKIE, state.auth.cookie.clear());
    Ok(res)
}

#[cfg(test)]
mod tests {
    use crate::auth::oidc::tests::{cfg, claims, provider, token};
    use crate::config::{AuthMode, SourcesConfig};
    use crate::state::tests::{dev_state, state};
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use std::collections::HashMap;
    use tower::ServiceExt;
    use url::Url;

    fn app(state: crate::state::AppState) -> Router {
        crate::app(state, None)
    }

    async fn send(app: &Router, req: Request<Body>) -> axum::response::Response {
        app.clone().oneshot(req).await.unwrap()
    }

    async fn get(app: &Router, uri: &str, cookie: Option<&str>) -> axum::response::Response {
        let mut req = Request::get(uri);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        send(app, req.body(Body::empty()).unwrap()).await
    }

    fn location(res: &axum::response::Response) -> String {
        res.headers()[header::LOCATION].to_str().unwrap().to_owned()
    }

    /// `name=value` from a Set-Cookie header.
    fn cookie(res: &axum::response::Response) -> String {
        res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned()
    }

    async fn body(res: axum::response::Response) -> String {
        String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn the_full_oidc_round_trip_keeps_the_search() {
        let p = provider("k1").await;
        let mut c = cfg(&p.issuer);
        c.allowed_groups = vec!["homelab-users".into()];
        let app = app(state(AuthMode::Oidc(c), SourcesConfig::default()));

        // Signed out: the API says 401.
        assert_eq!(get(&app, "/v1/me", None).await.status(), StatusCode::UNAUTHORIZED);

        // Login sends the browser to the provider with state and nonce.
        let res = get(&app, "/auth/login?return_to=%2Fsearch%3Fq%3Dtest", None).await;
        assert_eq!(res.status(), StatusCode::FOUND);
        let to = Url::parse(&location(&res)).unwrap();
        assert!(to.as_str().starts_with(&format!("{}/application/o/authorize/", p.base)));
        let q: HashMap<_, _> = to.query_pairs().into_owned().collect();
        assert_eq!(q["redirect_uri"], "http://localhost:1420/auth/callback");

        // The provider calls back; the token carries the nonce from login.
        *p.next_id_token.lock().unwrap() = Some(token("k1", claims(&p.issuer, &q["nonce"], &["homelab-users"])));
        let res = get(&app, &format!("/auth/callback?code=c1&state={}", q["state"]), None).await;
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(location(&res), "/search?q=test");
        let session = cookie(&res);
        assert!(session.starts_with("atlas_session="));

        let me = get(&app, "/v1/me", Some(&session)).await;
        assert_eq!(me.status(), StatusCode::OK);
        let me = body(me).await;
        assert!(me.contains(r#""username":"pwb""#), "{me}");

        // The state worked once.
        let again = get(&app, &format!("/auth/callback?code=c1&state={}", q["state"]), None).await;
        assert_eq!(again.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_user_outside_the_allowed_groups_gets_no_session() {
        let p = provider("k1").await;
        let mut c = cfg(&p.issuer);
        c.allowed_groups = vec!["homelab-users".into()];
        let app = app(state(AuthMode::Oidc(c), SourcesConfig::default()));

        let res = get(&app, "/auth/login", None).await;
        let q: HashMap<_, _> = Url::parse(&location(&res)).unwrap().query_pairs().into_owned().collect();
        *p.next_id_token.lock().unwrap() = Some(token("k1", claims(&p.issuer, &q["nonce"], &["guests"])));
        let res = get(&app, &format!("/auth/callback?code=c&state={}", q["state"]), None).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(!res.headers().contains_key(header::SET_COOKIE));
    }

    #[tokio::test]
    async fn a_token_for_another_sign_in_is_refused() {
        let p = provider("k1").await;
        let app = app(state(AuthMode::Oidc(cfg(&p.issuer)), SourcesConfig::default()));
        let res = get(&app, "/auth/login", None).await;
        let q: HashMap<_, _> = Url::parse(&location(&res)).unwrap().query_pairs().into_owned().collect();
        *p.next_id_token.lock().unwrap() = Some(token("k1", claims(&p.issuer, "someone-elses-nonce", &[])));
        let res = get(&app, &format!("/auth/callback?code=c&state={}", q["state"]), None).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(!res.headers().contains_key(header::SET_COOKIE));
    }

    #[tokio::test]
    async fn the_provider_refusing_shows_why() {
        let p = provider("k1").await;
        let app = app(state(AuthMode::Oidc(cfg(&p.issuer)), SourcesConfig::default()));
        let res = get(&app, "/auth/login", None).await;
        let q: HashMap<_, _> = Url::parse(&location(&res)).unwrap().query_pairs().into_owned().collect();
        let res = get(
            &app,
            &format!("/auth/callback?error=access_denied&error_description=Policy%20denied&state={}", q["state"]),
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(body(res).await.contains("Policy denied"));
    }

    #[tokio::test]
    async fn an_unreachable_provider_is_503() {
        let app = app(state(AuthMode::Oidc(cfg("http://127.0.0.1:9/application/o/atlas/")), SourcesConfig::default()));
        assert_eq!(get(&app, "/auth/login", None).await.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn return_to_cant_leave_atlas() {
        let app = app(dev_state());
        for bad in ["%2F%2Fevil.com", "%2F%5Cevil.com", "https%3A%2F%2Fevil.com"] {
            let res = get(&app, &format!("/auth/login?return_to={bad}"), None).await;
            assert_eq!(location(&res), "/", "{bad}");
        }
    }

    #[tokio::test]
    async fn dev_sign_in_and_logout() {
        let app = app(dev_state());
        let res = get(&app, "/auth/login?return_to=%2Fsearch%3Fq%3Dx", None).await;
        assert_eq!(location(&res), "/search?q=x");
        let session = cookie(&res);
        assert_eq!(get(&app, "/v1/me", Some(&session)).await.status(), StatusCode::OK);

        // Already signed in: straight through.
        let res = get(&app, "/auth/login?return_to=%2Fitem", Some(&session)).await;
        assert_eq!(location(&res), "/item");
        assert!(!res.headers().contains_key(header::SET_COOKIE));

        // Logout needs the CSRF header.
        let bare = Request::post("/auth/logout").header(header::COOKIE, &session).body(Body::empty()).unwrap();
        assert_eq!(send(&app, bare).await.status(), StatusCode::FORBIDDEN);

        let req = Request::post("/auth/logout")
            .header(header::COOKIE, &session)
            .header("x-atlas-request", "1")
            .body(Body::empty())
            .unwrap();
        let res = send(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
        assert_eq!(get(&app, "/v1/me", Some(&session)).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn disabled_sign_in_says_so() {
        let app = app(state(AuthMode::Disabled, SourcesConfig::default()));
        let res = get(&app, "/auth/login", None).await;
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
        assert!(body(res).await.contains("ATLAS_OIDC_ISSUER"));
    }
}

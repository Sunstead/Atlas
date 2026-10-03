//! `/v1/source-kinds` and `/v1/connections`: what a user can connect, and
//! their connections. Credentials go in and never come back out.

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::sources;
use crate::state::AppState;
use atlas_common::{ConnectionInfo, CreateConnection, ErrorCode, SourceKindInfo, UpdateConnection};
use atlas_state::{ConnectionPatch, ConnectionRow, NewConnection};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::json;

/// Longest label or credential accepted; anything longer is a mistake.
const MAX_LEN: usize = 4096;

fn info(c: &ConnectionRow) -> ConnectionInfo {
    ConnectionInfo {
        id: c.id,
        kind: c.kind.clone(),
        label: c.label.clone(),
        enabled: c.enabled,
        has_credential: c.credential.is_some(),
        created_at: c.created_at,
        updated_at: c.updated_at,
    }
}

fn clean(field: &str, value: Option<String>) -> Result<Option<String>, AppError> {
    let Some(v) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else { return Ok(None) };
    if v.len() > MAX_LEN {
        return Err(AppError::bad_request(format!("{field} is too long")));
    }
    Ok(Some(v))
}

fn need_key(state: &AppState) -> Result<(), AppError> {
    match state.master_key {
        Some(_) => Ok(()),
        None => Err(AppError::new(
            ErrorCode::NotEnabled,
            "This server can't store credentials: ATLAS_MASTER_KEY isn't set",
        )),
    }
}

pub async fn kinds(State(state): State<AppState>, _: CurrentUser) -> Json<Vec<SourceKindInfo>> {
    Json(sources::kinds(&state.sources))
}

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> Result<Json<Vec<ConnectionInfo>>, AppError> {
    Ok(Json(state.db.connections(user.id).await?.iter().map(info).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<CreateConnection>,
) -> Result<(StatusCode, Json<ConnectionInfo>), AppError> {
    let kind = sources::kind(&state.sources, &req.kind)
        .ok_or_else(|| AppError::bad_request(format!("Unknown source kind {:?}", req.kind)))?;
    if !kind.enabled {
        return Err(AppError::new(ErrorCode::NotEnabled, kind.disabled_reason.unwrap_or_default()));
    }
    let credential = clean("credential", req.credential)?;
    if credential.is_none() && kind.credential.as_ref().is_some_and(|c| c.required) {
        return Err(AppError::bad_request(format!("{} needs a credential", kind.name)));
    }
    if credential.is_some() {
        need_key(&state)?;
    }

    // Each user's file root is pinned when they connect, from the username
    // they signed in with. Later renames don't move it.
    let config = match (kind.kind.as_str(), &state.sources.opencloud) {
        (sources::OPENCLOUD, Some(oc)) => json!({ "root": oc.users_dir.join(&user.username) }),
        _ => json!({}),
    };
    let new = NewConnection {
        label: clean("label", req.label)?.unwrap_or(kind.name),
        kind: kind.kind,
        config,
        credential: credential.map(String::into_bytes),
    };
    let created = state.db.create_connection(user.id, new, state.master_key.clone()).await?;
    Ok((StatusCode::CREATED, Json(info(&created))))
}

pub async fn update(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<UpdateConnection>,
) -> Result<Json<ConnectionInfo>, AppError> {
    let credential = clean("credential", req.credential)?;
    if credential.is_some() {
        need_key(&state)?;
    }
    let patch = ConnectionPatch {
        label: clean("label", req.label)?,
        enabled: req.enabled,
        config: None,
        credential: credential.map(|c| Some(c.into_bytes())),
    };
    let updated = state.db.update_connection(user.id, id, patch, state.master_key.clone()).await?;
    Ok(Json(info(&updated)))
}

pub async fn remove(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    state.db.delete_connection(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use crate::config::{AuthMode, OpenCloudConfig, ServiceUrls, SourcesConfig};
    use crate::state::tests::state;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use std::path::PathBuf;
    use tower::ServiceExt;
    use url::Url;

    fn sources() -> SourcesConfig {
        let urls = |a: &str| ServiceUrls { api: Url::parse(a).unwrap(), public: Url::parse(a).unwrap() };
        SourcesConfig {
            immich: Some(urls("http://immich:2283")),
            opencloud: Some(OpenCloudConfig { urls: urls("http://opencloud:9200"), users_dir: PathBuf::from("/data/files/users") }),
        }
    }

    struct Client {
        app: Router,
        cookie: String,
    }

    impl Client {
        async fn signed_in(app: Router) -> Self {
            let res = app.clone().oneshot(Request::get("/auth/login").body(Body::empty()).unwrap()).await.unwrap();
            let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
            Self { app, cookie }
        }

        /// The signed-in user's rows, straight from the database.
        async fn rows(&self, s: &crate::state::AppState) -> Vec<atlas_state::ConnectionRow> {
            let token = self.cookie.split_once('=').unwrap().1;
            let session = s.db.session(token).await.unwrap().unwrap();
            s.db.connections(session.user).await.unwrap()
        }

        async fn call(&self, method: &str, uri: &str, body: Option<serde_json::Value>) -> (StatusCode, serde_json::Value) {
            let mut req = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, &self.cookie)
                .header("x-atlas-request", "1");
            if body.is_some() {
                req = req.header(header::CONTENT_TYPE, "application/json");
            }
            let req = req.body(body.map(|b| Body::from(b.to_string())).unwrap_or_default()).unwrap();
            let res = self.app.clone().oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
        }
    }

    fn dev_app(sources: SourcesConfig) -> (Router, crate::state::AppState) {
        let s = state(AuthMode::Dev { username: "pwb".into() }, sources);
        (crate::app(s.clone(), None), s)
    }

    #[tokio::test]
    async fn create_list_update_delete() {
        let (app, _) = dev_app(sources());
        let c = Client::signed_in(app).await;

        let (status, kinds) = c.call("GET", "/v1/source-kinds", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kinds.as_array().unwrap().len(), 2);

        let (status, created) =
            c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich", "credential": " key-123 " }))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["label"], "Immich");
        assert_eq!(created["has_credential"], true);
        assert!(!created.to_string().contains("key-123"), "the credential never comes back");

        let (_, list) = c.call("GET", "/v1/connections", None).await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert!(!list.to_string().contains("key-123"));

        let id = created["id"].as_i64().unwrap();
        let (status, updated) =
            c.call("PATCH", &format!("/v1/connections/{id}"), Some(serde_json::json!({ "label": "Family", "enabled": false }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["label"], "Family");
        assert_eq!(updated["enabled"], false);
        assert_eq!(updated["has_credential"], true, "untouched");

        let (status, _) = c.call("DELETE", &format!("/v1/connections/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = c.call("DELETE", &format!("/v1/connections/{id}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_credential_is_sealed_and_bound_to_the_row() {
        let (app, s) = dev_app(sources());
        let c = Client::signed_in(app).await;
        let (_, created) =
            c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich", "credential": "key-123" }))).await;
        let rows = c.rows(&s).await;
        let row = &rows[0];
        assert_eq!(row.id, created["id"].as_i64().unwrap());
        assert_eq!(s.db.open_credential(row, s.master_key.as_ref().unwrap()).unwrap().unwrap(), b"key-123");
    }

    #[tokio::test]
    async fn opencloud_pins_the_users_root() {
        let (app, s) = dev_app(sources());
        let c = Client::signed_in(app).await;
        let (status, _) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "opencloud" }))).await;
        assert_eq!(status, StatusCode::CREATED);
        let rows = c.rows(&s).await;
        let root = PathBuf::from(rows[0].config["root"].as_str().unwrap());
        assert_eq!(root, PathBuf::from("/data/files/users").join("pwb"));
    }

    #[tokio::test]
    async fn refuses_what_it_cant_do() {
        let (app, _) = dev_app(SourcesConfig::default());
        let c = Client::signed_in(app).await;
        let (status, e) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich", "credential": "k" }))).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "not configured");
        assert_eq!(e["code"], "not_enabled");

        let (app, _) = dev_app(sources());
        let c = Client::signed_in(app).await;
        let (status, _) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "dropbox" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "Immich needs a key");

        let (status, _) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich", "credential": "k" }))).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, e) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich", "credential": "k" }))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(e["code"], "conflict");
    }

    #[tokio::test]
    async fn without_a_master_key_credentials_are_refused() {
        let mut s = state(AuthMode::Dev { username: "pwb".into() }, sources());
        s.master_key = None;
        let c = Client::signed_in(crate::app(s, None)).await;
        let (status, _) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "immich", "credential": "k" }))).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        let (status, _) = c.call("POST", "/v1/connections", Some(serde_json::json!({ "kind": "opencloud" }))).await;
        assert_eq!(status, StatusCode::CREATED, "no credential, no key needed");
    }

    #[tokio::test]
    async fn signed_out_and_cross_site_requests_are_refused() {
        let (app, _) = dev_app(sources());
        let res = app.clone().oneshot(Request::get("/v1/connections").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let c = Client::signed_in(app.clone()).await;
        let post = |headers: &[(&str, &str)]| {
            let mut req = Request::post("/v1/connections")
                .header(header::COOKIE, &c.cookie)
                .header(header::CONTENT_TYPE, "application/json");
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            req.body(Body::from(r#"{"kind":"opencloud"}"#)).unwrap()
        };
        let res = app.clone().oneshot(post(&[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "no X-Atlas-Request");
        let res = app.clone().oneshot(post(&[("x-atlas-request", "1"), ("origin", "https://evil.example")])).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "foreign origin");
        let res = app.clone().oneshot(post(&[("x-atlas-request", "1"), ("origin", "http://localhost:1420")])).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
    }
}

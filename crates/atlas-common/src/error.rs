use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Machine-readable failure reason. The client keys its behaviour off this,
/// not off the HTTP status. Same split as Cosmos: `NotEnabled` means "hide
/// it, retrying never helps", `Unavailable` means "show a retry".
#[derive(Serialize, Deserialize, TS, Debug, Clone, Copy, PartialEq, Eq)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    /// No session, or it expired. The web app sends the user to sign in.
    Unauthorized,
    Forbidden,
    NotFound,
    /// Clashes with what exists, e.g. a second connection of one kind.
    Conflict,
    /// Feature or source disabled in config. Hide it.
    NotEnabled,
    /// Configured but failing right now (the identity provider, a source).
    Unavailable,
    Internal,
}

#[derive(Serialize, Deserialize, TS, Debug, Clone)]
#[ts(export)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_snake_case_on_the_wire() {
        let e = ApiError { code: ErrorCode::NotEnabled, message: "off".into(), detail: None };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"code":"not_enabled","message":"off","detail":null}"#
        );
    }
}

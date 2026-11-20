//! Errors the UI can act on: a kind to branch on and a message to show.

use serde::Serialize;

use crate::api;
use crate::auth;
use crate::sandbox::SandboxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// The free plan's one sandbox exists. Offer Connect on it.
    FreeLimit,
    /// `minutes` says how long.
    Cooldown,
    /// The ADE is busy on the sandbox. The UI knows it as `t3Busy`.
    #[serde(rename = "t3Busy")]
    AdeBusy,
    Setup,
    SignedOut,
    Timeout,
    /// An API error; `code` is the API's error code.
    Api,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorView {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ErrorView {
    fn new(kind: ErrorKind, message: String) -> Self {
        ErrorView {
            kind,
            message,
            minutes: None,
            code: None,
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other, message.into())
    }

    pub fn from_auth(e: &auth::Error) -> Self {
        match e {
            auth::Error::Api(a) => ErrorView::from(a.clone()),
            other => ErrorView::other(other.to_string()),
        }
    }
}

impl From<api::Error> for ErrorView {
    fn from(e: api::Error) -> Self {
        let kind = if e.status == 401 || e.code == "unauthenticated" {
            ErrorKind::SignedOut
        } else {
            ErrorKind::Api
        };
        ErrorView {
            kind,
            message: e.message,
            minutes: None,
            code: Some(e.code),
        }
    }
}

impl From<SandboxError> for ErrorView {
    fn from(e: SandboxError) -> Self {
        let message = e.to_string();
        match e {
            SandboxError::FreeLimit => ErrorView::new(ErrorKind::FreeLimit, message),
            SandboxError::Cooldown { minutes } => ErrorView {
                minutes: Some(minutes),
                ..ErrorView::new(ErrorKind::Cooldown, message)
            },
            SandboxError::AdeBusy(_) => ErrorView::new(ErrorKind::AdeBusy, message),
            SandboxError::Setup { .. } => ErrorView::new(ErrorKind::Setup, message),
            SandboxError::Timeout => ErrorView::new(ErrorKind::Timeout, message),
            SandboxError::FixedOnFree | SandboxError::NewerThanDesktop(_) => {
                ErrorView::other(message)
            }
            SandboxError::Api(a) => ErrorView::from(a),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ade::Ade;

    fn api_err(status: u16, code: &str) -> api::Error {
        api::Error {
            status,
            code: code.into(),
            message: "msg".into(),
            retryable: false,
            retry_after: None,
        }
    }

    #[test]
    fn sandbox_errors_map_to_kinds() {
        let v = ErrorView::from(SandboxError::FreeLimit);
        assert_eq!(v.kind, ErrorKind::FreeLimit);
        assert_eq!(v.message, "The free plan has one sandbox");

        let v = ErrorView::from(SandboxError::Cooldown { minutes: 12 });
        assert_eq!((v.kind, v.minutes), (ErrorKind::Cooldown, Some(12)));
        assert_eq!(v.message, "Cooling down, ready in about 12 min");

        assert_eq!(
            ErrorView::from(SandboxError::AdeBusy(Ade::T3)).kind,
            ErrorKind::AdeBusy
        );
        let v = ErrorView::from(SandboxError::Setup {
            ade: Ade::T3,
            stderr_tail: "boom".into(),
        });
        assert_eq!(v.kind, ErrorKind::Setup);
        assert!(v.message.contains("boom"));
        assert_eq!(
            ErrorView::from(SandboxError::Timeout).kind,
            ErrorKind::Timeout
        );
        assert_eq!(
            ErrorView::from(SandboxError::FixedOnFree).kind,
            ErrorKind::Other
        );
    }

    #[test]
    fn api_errors_carry_their_code_and_401_is_signed_out() {
        let v = ErrorView::from(SandboxError::Api(api_err(422, "bad_spec")));
        assert_eq!(v.kind, ErrorKind::Api);
        assert_eq!(v.code.as_deref(), Some("bad_spec"));
        assert_eq!(
            ErrorView::from(api_err(401, "http_401")).kind,
            ErrorKind::SignedOut
        );
        assert_eq!(
            ErrorView::from(api_err(0, "unauthenticated")).kind,
            ErrorKind::SignedOut
        );
        assert_eq!(
            ErrorView::from_auth(&auth::Error::Expired).kind,
            ErrorKind::Other
        );
    }

    #[test]
    fn json_is_camel_case_and_omits_empty_fields() {
        let v = serde_json::to_value(ErrorView::from(SandboxError::AdeBusy(Ade::T3))).unwrap();
        assert_eq!(v["kind"], "t3Busy");
        assert!(v.get("minutes").is_none() && v.get("code").is_none());
        let v = serde_json::to_value(ErrorView::from(SandboxError::FreeLimit)).unwrap();
        assert_eq!(v["kind"], "freeLimit");
        let v =
            serde_json::to_value(ErrorView::from(SandboxError::Cooldown { minutes: 3 })).unwrap();
        assert_eq!(v["minutes"], 3);
    }
}

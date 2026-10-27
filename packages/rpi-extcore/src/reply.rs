//! Protocol-independent outcomes; adapters select status/error codes.
use serde_json::{json, Value};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyKind {
    Success,
    Created,
    InvalidInput,
    NotFound,
    AccessDenied,
    LimitExceeded,
    UnsupportedContent,
    Internal,
    HostUnavailable,
    Timeout,
    Unsupported,
    Conflict,
    Empty,
    Unavailable,
}
pub type ServiceError = (ReplyKind, Value);
pub type ServiceResult<T = Value> = Result<T, ServiceError>;
#[derive(Debug)]
pub struct ServiceReply {
    pub kind: ReplyKind,
    pub value: Value,
}
pub trait IntoReply {
    fn into_reply(self) -> ServiceReply;
}
impl IntoReply for ReplyKind {
    fn into_reply(self) -> ServiceReply {
        ServiceReply {
            kind: self,
            value: Value::Null,
        }
    }
}
impl IntoReply for ServiceReply {
    fn into_reply(self) -> ServiceReply {
        self
    }
}
impl IntoReply for Value {
    fn into_reply(self) -> ServiceReply {
        ServiceReply {
            kind: ReplyKind::Success,
            value: self,
        }
    }
}
impl IntoReply for (ReplyKind, Value) {
    fn into_reply(self) -> ServiceReply {
        ServiceReply {
            kind: self.0,
            value: self.1,
        }
    }
}
impl IntoReply for ServiceResult {
    fn into_reply(self) -> ServiceReply {
        match self {
            Ok(value) => value.into_reply(),
            Err(error) => error.into_reply(),
        }
    }
}
pub fn failure(kind: ReplyKind, message: &str) -> ServiceError {
    (kind, json!({"error":message}))
}

//! Handler registry: one async handler per method name of [`crate::api::METHODS`].
//!
//! `Handler = Arc<dyn Fn(Value, Ctx) -> BoxFuture<Result<Value>>>`: raw JSON in, raw
//! JSON out. [`Router::handle`] is the typed way to register: params are deserialized
//! into `M::Params` (failure → `invalid_input`) and the result serialized from
//! `M::Result`, so a handler cannot answer with the wrong shape.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::ids::{RequestId, SessionId, SubscriptionId};

use super::outbox::{Outbox, StreamSender};
use crate::api::{ApiMethod, ClientInfoDto, MethodInfo, MethodKind, Role, method_info};
use crate::transport::ConnectionInfo;

/// A raw handler.
pub type Handler = Arc<dyn Fn(Value, Ctx) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Route {
    pub(crate) handler: Handler,
    pub(crate) timeout: Option<Duration>,
}

/// Method name → handler.
#[derive(Clone, Default)]
pub struct Router {
    routes: HashMap<&'static str, Route>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<_> = self.routes.keys().collect();
        names.sort();
        f.debug_struct("Router").field("methods", &names).finish()
    }
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a typed handler for `M` (replaces a previous one).
    pub fn handle<M, F, Fut>(&mut self, handler: F) -> &mut Self
    where
        M: ApiMethod,
        F: Fn(M::Params, Ctx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Result>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let raw: Handler = Arc::new(move |params: Value, ctx: Ctx| {
            let handler = Arc::clone(&handler);
            async move {
                let params: M::Params = serde_json::from_value(params).map_err(|e| {
                    ForgeError::InvalidInput(format!("invalid params for {}: {e}", M::INFO.name))
                })?;
                let result = handler(params, ctx).await?;
                serde_json::to_value(result).map_err(|e| {
                    ForgeError::Internal(format!("{} result not serializable: {e}", M::INFO.name))
                })
            }
            .boxed()
        });
        self.routes.insert(
            M::INFO.name,
            Route {
                handler: raw,
                timeout: None,
            },
        );
        self
    }

    /// Registers a raw handler. `name` must be a method of the table.
    pub fn handle_raw(&mut self, name: &str, handler: Handler) -> Result<&mut Self> {
        let info = method_info(name)
            .ok_or_else(|| ForgeError::InvalidInput(format!("unknown API method {name:?}")))?;
        self.routes.insert(
            info.name,
            Route {
                handler,
                timeout: None,
            },
        );
        Ok(self)
    }

    /// Overrides the answer deadline of `M` (default: `ServerConfig::request_timeout`).
    pub fn timeout<M: ApiMethod>(&mut self, timeout: Duration) -> &mut Self {
        if let Some(route) = self.routes.get_mut(M::INFO.name) {
            route.timeout = Some(timeout);
        }
        self
    }

    /// Whether `name` has a handler.
    pub fn contains(&self, name: &str) -> bool {
        self.routes.contains_key(name)
    }

    /// Names with a handler, sorted.
    pub fn methods(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.routes.keys().copied().collect();
        names.sort_unstable();
        names
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Route> {
        self.routes.get(name)
    }

    /// Adds the routes of `other` that this router does not have yet.
    pub(crate) fn merge_missing(&mut self, other: Router) {
        for (name, route) in other.routes {
            self.routes.entry(name).or_insert(route);
        }
    }
}

/// An authenticated connection.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub role: Role,
    pub client: ClientInfoDto,
    pub connection: ConnectionInfo,
    pub connected_at: Timestamp,
}

/// Context of one request.
#[derive(Clone)]
pub struct Ctx {
    pub(crate) request_id: RequestId,
    pub(crate) method: &'static MethodInfo,
    pub(crate) session: Arc<Session>,
    pub(crate) outbox: Arc<Outbox>,
    pub(crate) opened: Arc<OnceLock<SubscriptionId>>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("request_id", &self.request_id)
            .field("method", &self.method.name)
            .field("session", &self.session.id)
            .finish()
    }
}

impl Ctx {
    /// Idempotency key of the request (pass it to `JobSpec::request_id`).
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn method(&self) -> &'static MethodInfo {
        self.method
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Opens the stream of a `stream` method. Items queued before the subscribe answer
    /// is sent are held back and delivered right after it. One stream per request.
    pub fn open_stream(&self, capacity: Option<usize>) -> Result<StreamSender> {
        if self.method.kind != MethodKind::Stream {
            return Err(ForgeError::Internal(format!(
                "{} is not a stream method",
                self.method.name
            )));
        }
        let (Some(notification), Some(lagged)) =
            (self.method.notification, self.method.lagged_notification)
        else {
            return Err(ForgeError::Internal(format!(
                "{} has no notification names",
                self.method.name
            )));
        };
        let (id, token) = self.outbox.open_stream(lagged, capacity)?;
        if self.opened.set(id).is_err() {
            self.outbox.close_stream(id);
            return Err(ForgeError::Internal(format!(
                "{} opened two streams",
                self.method.name
            )));
        }
        Ok(StreamSender::new(
            id,
            notification,
            Arc::clone(&self.outbox),
            token,
        ))
    }

    /// Closes a stream of this connection (`*.unsubscribe`). `false` if unknown.
    pub fn close_stream(&self, id: SubscriptionId) -> bool {
        self.outbox.close_stream(id)
    }
}

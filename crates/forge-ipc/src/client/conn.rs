//! One live client connection: a writer task, a reader task that routes responses to
//! waiting calls and notifications to subscriptions, and the `system.hello` handshake.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use forge_core::ids::{RequestId, SubscriptionId};
use forge_core::sync::{LockLevel, OrderedMutex};

use super::ClientError;
use super::subscription::SubShared;
use crate::api::{API_VERSION, HelloDto, HelloParams, SubscriptionDto};
use crate::rpc::{ServerMessage, parse_server_message, request_frame};
use crate::transport::{Connection, ConnectionInfo, FrameSink, FrameStream, Transport};

/// Frames queued for the writer before `request` waits.
const OUTBOUND_CAPACITY: usize = 256;

struct Pending {
    reply: oneshot::Sender<Result<Value, ClientError>>,
    /// Subscribe requests: the subscription to attach when the answer arrives.
    route: Option<Arc<SubShared>>,
}

pub(crate) struct Conn {
    pub(crate) generation: u64,
    pub(crate) info: ConnectionInfo,
    hello: OnceLock<HelloDto>,
    outbound: mpsc::Sender<String>,
    /// Lock: `ipc.client` (rank 10), never nested with `routes`.
    pending: OrderedMutex<HashMap<u64, Pending>>,
    /// Lock: `ipc.client` (rank 10), never nested with `pending`.
    routes: OrderedMutex<HashMap<SubscriptionId, Arc<SubShared>>>,
    next_id: AtomicU64,
    /// `system.hello` succeeded: losing the connection from now on wakes the client's
    /// maintenance task (a failed handshake is reported by the attempt itself).
    established: AtomicBool,
    pub(crate) closed: CancellationToken,
}

impl Conn {
    pub(crate) fn hello(&self) -> Option<&HelloDto> {
        self.hello.get()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Sends a request and waits for its answer (at most `timeout`).
    pub(crate) async fn request(
        &self,
        method: &str,
        params: &Value,
        request_id: RequestId,
        timeout: Duration,
        route: Option<Arc<SubShared>>,
    ) -> Result<Value, ClientError> {
        if self.is_closed() {
            return Err(ClientError::Disconnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = request_frame(id, method, request_id, params)
            .map_err(|e| ClientError::Protocol(format!("params of {method}: {e}")))?;
        let (reply, answer) = oneshot::channel();
        self.pending.lock().insert(id, Pending { reply, route });
        // The reader cancels `closed` before failing the pending calls: if it is not
        // cancelled now, this entry will be answered or failed.
        if self.is_closed() {
            self.pending.lock().remove(&id);
            return Err(ClientError::Disconnected);
        }
        let exchange = async {
            self.outbound
                .send(frame)
                .await
                .map_err(|_| ClientError::Disconnected)?;
            answer.await.map_err(|_| ClientError::Disconnected)?
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(result) => {
                if result.is_err() {
                    self.pending.lock().remove(&id);
                }
                result
            }
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(ClientError::Timeout(timeout))
            }
        }
    }

    /// Fire-and-forget request (no answer awaited); used from `Drop`.
    pub(crate) fn send_detached(&self, method: &str, params: &Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(frame) = request_frame(id, method, RequestId::new(), params) {
            let _ = self.outbound.try_send(frame);
        }
    }

    fn fail_all(&self) {
        let pending: Vec<Pending> = self.pending.lock().drain().map(|(_, p)| p).collect();
        for p in pending {
            let _ = p.reply.send(Err(ClientError::Disconnected));
        }
        self.routes.lock().clear();
    }

    fn on_response(&self, id: Option<u64>, outcome: Result<Value, crate::rpc::RpcErrorObject>) {
        let Some(id) = id else {
            if let Err(error) = outcome {
                tracing::warn!(message = %error.message, "API error without request id");
            }
            return;
        };
        let Some(pending) = self.pending.lock().remove(&id) else {
            // Answer to a timed-out or detached request.
            return;
        };
        let result = outcome.map_err(|e| ClientError::Api(e.into_api_error()));
        if let (Ok(value), Some(sub)) = (&result, &pending.route) {
            match serde_json::from_value::<SubscriptionDto>(value.clone()) {
                Ok(dto) => {
                    // Attach before handing the answer over: the next frame may already
                    // be a notification of this subscription.
                    sub.attach(self.generation, &dto);
                    self.routes
                        .lock()
                        .insert(dto.subscription_id, Arc::clone(sub));
                }
                Err(e) => {
                    let _ = pending.reply.send(Err(ClientError::Protocol(format!(
                        "invalid subscription answer: {e}"
                    ))));
                    return;
                }
            }
        }
        let _ = pending.reply.send(result);
    }

    fn on_notification(&self, method: &str, params: Value) {
        let Some(id) = params
            .get("subscription_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<SubscriptionId>().ok())
        else {
            tracing::debug!(method, "notification without subscription id ignored");
            return;
        };
        let Some(sub) = self.routes.lock().get(&id).cloned() else {
            return;
        };
        if method == sub.lagged_method() {
            let missed = params.get("missed").and_then(Value::as_u64).unwrap_or(0);
            sub.lagged(missed);
        } else if method == sub.item_method() {
            sub.deliver(params);
        }
    }

    /// Unregisters a route (after an unsubscribe).
    pub(crate) fn forget_route(&self, id: SubscriptionId) {
        self.routes.lock().remove(&id);
    }
}

async fn write_loop(
    mut sink: FrameSink,
    mut outbound: mpsc::Receiver<String>,
    closed: CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            () = closed.cancelled() => break,
            frame = outbound.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
        let write = async {
            sink.feed(frame).await?;
            while let Ok(more) = outbound.try_recv() {
                sink.feed(more).await?;
            }
            sink.flush().await
        };
        let written = tokio::select! {
            () = closed.cancelled() => break,
            written = write => written,
        };
        if let Err(err) = written {
            tracing::debug!(%err, "API client write failed");
            closed.cancel();
            break;
        }
    }
    let _ = tokio::time::timeout(Duration::from_millis(200), sink.close()).await;
}

async fn read_loop(conn: Arc<Conn>, mut stream: FrameStream, lost: Arc<Notify>) {
    loop {
        let frame = tokio::select! {
            () = conn.closed.cancelled() => break,
            frame = stream.next() => frame,
        };
        let frame = match frame {
            Some(Ok(frame)) => frame,
            Some(Err(err)) => {
                tracing::debug!(%err, "API client read failed");
                break;
            }
            None => break,
        };
        match parse_server_message(&frame) {
            Ok(ServerMessage::Response { id, outcome }) => conn.on_response(id, outcome),
            Ok(ServerMessage::Notification { method, params }) => {
                conn.on_notification(&method, params);
            }
            Err(err) => tracing::warn!(%err, "invalid frame from the daemon"),
        }
    }
    // Cancel before waking: the client checks `closed` to never report a lost
    // connection as connected after the loss was reported.
    conn.closed.cancel();
    conn.fail_all();
    if conn.established.load(Ordering::SeqCst) {
        lost.notify_one();
    }
}

/// Opens a connection and authenticates it.
pub(crate) async fn connect(
    transport: Arc<dyn Transport>,
    hello: HelloParams,
    connect_timeout: Duration,
    generation: u64,
    lost: Arc<Notify>,
) -> Result<Arc<Conn>, ClientError> {
    let connection: Connection = tokio::time::timeout(connect_timeout, transport.connect())
        .await
        .map_err(|_| ClientError::Timeout(connect_timeout))?
        .map_err(ClientError::Transport)?;
    let Connection { info, sink, stream } = connection;
    let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_CAPACITY);
    let closed = CancellationToken::new();
    let conn = Arc::new(Conn {
        generation,
        info,
        hello: OnceLock::new(),
        outbound: outbound_tx,
        pending: OrderedMutex::new(LockLevel::IPC_CLIENT, HashMap::new()),
        routes: OrderedMutex::new(LockLevel::IPC_CLIENT, HashMap::new()),
        next_id: AtomicU64::new(1),
        established: AtomicBool::new(false),
        closed: closed.clone(),
    });
    tokio::spawn(write_loop(sink, outbound_rx, closed.clone()));
    tokio::spawn(read_loop(Arc::clone(&conn), stream, lost));

    let params = serde_json::to_value(&hello)
        .map_err(|e| ClientError::Protocol(format!("hello params: {e}")))?;
    let answer = conn
        .request(
            "system.hello",
            &params,
            RequestId::new(),
            connect_timeout,
            None,
        )
        .await;
    let dto = answer.and_then(|value| {
        serde_json::from_value::<HelloDto>(value)
            .map_err(|e| ClientError::Protocol(format!("invalid hello answer: {e}")))
    });
    let dto = match dto {
        Ok(dto) => dto,
        Err(err) => {
            closed.cancel();
            return Err(err);
        }
    };
    if !API_VERSION.is_compatible_with(dto.api_version) {
        closed.cancel();
        return Err(ClientError::Protocol(format!(
            "daemon speaks API {}, this client {API_VERSION}",
            dto.api_version
        )));
    }
    let _ = conn.hello.set(dto);
    conn.established.store(true, Ordering::SeqCst);
    if conn.is_closed() {
        // Lost right after the handshake, before the reader could see `established`.
        return Err(ClientError::Disconnected);
    }
    Ok(conn)
}

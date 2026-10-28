//! One connection: reader (parse, `system.hello`, dispatch) and writer tasks.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, OnceLock};

use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use forge_core::Timestamp;
use forge_core::actor::panic_message;
use forge_core::ids::{RequestId, SessionId};

use super::ServerShared;
use super::idempotency::{Begin, Outcome};
use super::outbox::Outbox;
use super::router::{Ctx, Session};
use crate::api::{
    API_VERSION, ApiError, HelloDto, HelloParams, MethodKind, SubscriptionDto, method_info,
};
use crate::rpc::{
    ClientMessage, RpcErrorObject, code_for, codes, parse_client_message, response_err, response_ok,
};
use crate::transport::{Connection, ConnectionInfo, FrameSink, TransportError};

/// Frames written per batch before flushing.
const WRITE_BATCH: usize = 64;

fn error_frame(id: &Value, code: i64, error: ApiError) -> String {
    response_err(id, &RpcErrorObject::new(code, error))
}

fn outcome_frame(id: &Value, outcome: &Outcome) -> String {
    match outcome {
        Ok(value) => response_ok(id, value),
        Err(error) => error_frame(id, code_for(error), error.clone()),
    }
}

/// Serves one connection until it closes, the client misbehaves or the server stops.
pub(crate) async fn run(
    shared: Arc<ServerShared>,
    connection: Connection,
    conn_id: u64,
    cancel: CancellationToken,
) {
    let Connection { info, sink, stream } = connection;
    let config = &shared.info.config;
    let outbox = Arc::new(Outbox::new(
        cancel.clone(),
        config.stream_queue_capacity,
        config.max_subscriptions,
    ));
    let writer = tokio::spawn(write_loop(sink, Arc::clone(&outbox), cancel.clone()));
    let permits = Arc::new(Semaphore::new(config.max_in_flight));
    tracing::debug!(conn_id, peer = %info.peer, "API connection opened");

    read_loop(&shared, &info, stream, &outbox, &permits, &cancel).await;

    if !cancel.is_cancelled() {
        // Let the answers already being computed reach the client, within a bound
        // (permits come back once their answer is written).
        let max = u32::try_from(config.max_in_flight).unwrap_or(u32::MAX);
        let _ = tokio::time::timeout(config.drain_timeout, async {
            tokio::select! {
                _ = permits.acquire_many(max) => {}
                () = cancel.cancelled() => {}
            }
        })
        .await;
        outbox.close_after_drain();
        if tokio::time::timeout(config.drain_timeout, writer)
            .await
            .is_err()
        {
            tracing::debug!(conn_id, "writer did not drain in time");
        }
    }
    cancel.cancel();
    outbox.close();
    shared.unregister(conn_id);
    tracing::debug!(conn_id, peer = %info.peer, "API connection closed");
}

async fn read_loop(
    shared: &Arc<ServerShared>,
    info: &ConnectionInfo,
    mut stream: crate::transport::FrameStream,
    outbox: &Arc<Outbox>,
    permits: &Arc<Semaphore>,
    cancel: &CancellationToken,
) {
    let config = &shared.info.config;
    let hello_deadline = tokio::time::Instant::now() + config.hello_timeout;
    let mut session: Option<Arc<Session>> = None;
    loop {
        let next = if session.is_none() {
            tokio::select! {
                () = cancel.cancelled() => return,
                next = tokio::time::timeout_at(hello_deadline, stream.next()) => match next {
                    Ok(next) => next,
                    Err(_) => {
                        tracing::debug!(peer = %info.peer, "no system.hello in time; closing");
                        return;
                    }
                },
            }
        } else {
            tokio::select! {
                () = cancel.cancelled() => return,
                next = stream.next() => next,
            }
        };
        let frame = match next {
            None => return,
            Some(Ok(frame)) => frame,
            Some(Err(
                err @ (TransportError::FrameTooLarge { .. } | TransportError::InvalidFrame(_)),
            )) => {
                tracing::warn!(peer = %info.peer, %err, "protocol violation; closing");
                outbox.push_response(
                    error_frame(
                        &Value::Null,
                        codes::INVALID_REQUEST,
                        ApiError::new("invalid_input", err.to_string()),
                    ),
                    None,
                    None,
                );
                return;
            }
            Some(Err(err)) => {
                tracing::debug!(peer = %info.peer, %err, "connection read failed");
                return;
            }
        };
        // Backpressure: one permit per request running or waiting to be written.
        let permit = tokio::select! {
            () = cancel.cancelled() => return,
            permit = Arc::clone(permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
        };
        let message = match parse_client_message(&frame) {
            Ok(message) => message,
            Err(invalid) => {
                outbox.push_response(
                    response_err(&invalid.id, &invalid.error),
                    Some(permit),
                    None,
                );
                continue;
            }
        };
        let ClientMessage::Request {
            id,
            method,
            request_id,
            request_id_raw,
            params,
        } = message
        else {
            // Client notifications are not part of the API: ignored (JSON-RPC §4.1).
            continue;
        };
        match &session {
            None => {
                if method != "system.hello" {
                    outbox.push_response(
                        error_frame(
                            &id,
                            codes::UNAUTHORIZED,
                            ApiError::new("unauthorized", "send system.hello first"),
                        ),
                        Some(permit),
                        None,
                    );
                    continue;
                }
                match hello(shared, info, params, request_id, request_id_raw) {
                    Ok((new_session, dto)) => {
                        let value = serde_json::to_value(&dto).unwrap_or(Value::Null);
                        outbox.push_response(response_ok(&id, &value), Some(permit), None);
                        tracing::debug!(
                            peer = %info.peer,
                            client = %new_session.client.name,
                            role = %new_session.role,
                            "API client authenticated"
                        );
                        session = Some(new_session);
                    }
                    Err((code, error)) => {
                        let fatal = error.code == "unauthorized";
                        outbox.push_response(error_frame(&id, code, error), Some(permit), None);
                        if fatal {
                            tracing::warn!(peer = %info.peer, "API hello refused (invalid token)");
                            return;
                        }
                    }
                }
            }
            Some(session) => dispatch(
                shared,
                session,
                outbox,
                permit,
                id,
                method,
                request_id,
                request_id_raw,
                params,
            ),
        }
    }
}

type HelloError = (i64, ApiError);

fn hello(
    shared: &ServerShared,
    info: &ConnectionInfo,
    params: Value,
    request_id: Option<RequestId>,
    request_id_raw: Option<Value>,
) -> Result<(Arc<Session>, HelloDto), HelloError> {
    check_request_id(request_id, request_id_raw.as_ref())?;
    let hello: HelloParams = serde_json::from_value(params).map_err(|e| {
        (
            codes::INVALID_PARAMS,
            ApiError::new("invalid_input", format!("invalid system.hello params: {e}")),
        )
    })?;
    if !API_VERSION.is_compatible_with(hello.api_version) {
        return Err((
            codes::INVALID_REQUEST,
            ApiError::new(
                "invalid_input",
                format!(
                    "API {} is not supported by this daemon (speaks {API_VERSION})",
                    hello.api_version
                ),
            ),
        ));
    }
    let role = shared
        .auth
        .authenticate(&hello, info)
        .map_err(|e| (codes::UNAUTHORIZED, ApiError::from(&e)))?;
    let session = Arc::new(Session {
        id: SessionId::new(),
        role,
        client: hello.client,
        connection: info.clone(),
        connected_at: Timestamp::now(),
    });
    Ok((Arc::clone(&session), hello_dto(shared, &session)))
}

fn hello_dto(shared: &ServerShared, session: &Session) -> HelloDto {
    HelloDto {
        session_id: session.id,
        api_version: API_VERSION,
        daemon_version: shared.info.config.daemon_version.clone(),
        platform: std::env::consts::OS.to_owned(),
        role: session.role,
        transport: session.connection.transport,
        local: session.connection.local,
        last_event_seq: shared
            .info
            .events
            .as_ref()
            .map_or(0, |events| events.bus().last_seq()),
        server_time: Timestamp::now(),
        not_implemented: shared.info.not_implemented.clone(),
    }
}

fn check_request_id(
    request_id: Option<RequestId>,
    raw: Option<&Value>,
) -> Result<RequestId, HelloError> {
    request_id.ok_or_else(|| {
        let message = match raw {
            None => "missing \"request_id\" (a ULID, required by every request)".to_owned(),
            Some(raw) => format!("\"request_id\" must be a ULID string, got {raw}"),
        };
        (
            codes::INVALID_REQUEST,
            ApiError::new("invalid_input", message),
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    shared: &Arc<ServerShared>,
    session: &Arc<Session>,
    outbox: &Arc<Outbox>,
    permit: OwnedSemaphorePermit,
    id: Value,
    method: String,
    request_id: Option<RequestId>,
    request_id_raw: Option<Value>,
    params: Value,
) {
    let reject = |code: i64, error: ApiError, permit: OwnedSemaphorePermit| {
        outbox.push_response(error_frame(&id, code, error), Some(permit), None);
    };
    let Some(info) = method_info(&method) else {
        return reject(
            codes::METHOD_NOT_FOUND,
            ApiError::new("not_found", format!("unknown method {method:?}")),
            permit,
        );
    };
    let request_id = match check_request_id(request_id, request_id_raw.as_ref()) {
        Ok(request_id) => request_id,
        Err((code, error)) => return reject(code, error, permit),
    };
    if info.name == "system.hello" {
        // Already authenticated: answer again (idempotent).
        let dto = hello_dto(shared, session);
        let value = serde_json::to_value(&dto).unwrap_or(Value::Null);
        outbox.push_response(response_ok(&id, &value), Some(permit), None);
        return;
    }
    if !session.role.allows(info.min_role) {
        return reject(
            codes::FORBIDDEN,
            ApiError::new(
                "forbidden",
                format!(
                    "{} requires role {} (this connection is {})",
                    info.name, info.min_role, session.role
                ),
            ),
            permit,
        );
    }
    if info.local_only && !session.connection.local {
        return reject(
            codes::FORBIDDEN,
            ApiError::new(
                "forbidden",
                format!(
                    "{} is local_only: it acts on the daemon's machine and is not available to remote clients",
                    info.name
                ),
            ),
            permit,
        );
    }
    let Some(route) = shared.router.get(info.name).cloned() else {
        return reject(
            codes::SERVER_ERROR,
            ApiError::new(
                "not_implemented",
                format!("{} is not implemented by this daemon yet", info.name),
            ),
            permit,
        );
    };
    let ctx = Ctx {
        request_id,
        method: info,
        session: Arc::clone(session),
        outbox: Arc::clone(outbox),
        opened: Arc::new(OnceLock::new()),
    };
    let shared = Arc::clone(shared);
    let outbox = Arc::clone(outbox);
    let deadline = route.timeout.unwrap_or(shared.info.config.request_timeout);
    tokio::spawn(async move {
        let guard = if info.mutating {
            match shared.idempotency.begin(request_id, info.name) {
                Begin::Run(guard) => Some(guard),
                Begin::Cached(outcome) => {
                    outbox.push_response(outcome_frame(&id, &outcome), Some(permit), None);
                    return;
                }
                Begin::Wait(mut done) => {
                    let waited =
                        tokio::time::timeout(deadline, done.wait_for(Option::is_some)).await;
                    let outcome = match waited {
                        Ok(Ok(outcome)) => outcome.clone().unwrap_or_else(|| {
                            Err(ApiError::new("internal", "empty idempotent outcome"))
                        }),
                        _ => Err(ApiError::new(
                            "timeout",
                            format!("{} with this request_id is still running", info.name),
                        )),
                    };
                    outbox.push_response(outcome_frame(&id, &outcome), Some(permit), None);
                    return;
                }
                Begin::Mismatch(other) => {
                    let error = ApiError::new(
                        "invalid_input",
                        format!("request_id {request_id} was already used for {other}"),
                    );
                    outbox.push_response(
                        error_frame(&id, codes::INVALID_REQUEST, error),
                        Some(permit),
                        None,
                    );
                    return;
                }
            }
        } else {
            None
        };
        let opened = Arc::clone(&ctx.opened);
        let future = AssertUnwindSafe((route.handler)(params, ctx)).catch_unwind();
        let outcome: Outcome = match tokio::time::timeout(deadline, future).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(err))) => Err(ApiError::from(&err)),
            Ok(Err(panic)) => {
                let message = panic_message(&*panic);
                tracing::error!(method = info.name, %message, "API handler panicked");
                Err(ApiError::new(
                    "internal",
                    format!("{} failed unexpectedly: {message}", info.name),
                ))
            }
            Err(_) => Err(ApiError::new(
                "timeout",
                format!(
                    "{} did not answer within {} ms",
                    info.name,
                    deadline.as_millis()
                ),
            )),
        };
        let stream = opened.get().copied();
        let activate = match (&outcome, stream) {
            (Ok(_), Some(sub)) if info.kind == MethodKind::Stream => Some(sub),
            (_, Some(sub)) => {
                outbox.close_stream(sub);
                None
            }
            _ => None,
        };
        let outcome = check_stream_answer(info.kind, outcome, activate);
        let frame = outcome_frame(&id, &outcome);
        if let Some(guard) = guard {
            guard.complete(outcome);
        }
        outbox.push_response(frame, Some(permit), activate);
    });
}

/// A stream method must open a stream and answer with its subscription.
fn check_stream_answer(
    kind: MethodKind,
    outcome: Outcome,
    opened: Option<forge_core::ids::SubscriptionId>,
) -> Outcome {
    if kind != MethodKind::Stream {
        return outcome;
    }
    let value = outcome?;
    match serde_json::from_value::<SubscriptionDto>(value.clone()) {
        Ok(dto) if Some(dto.subscription_id) == opened => Ok(value),
        _ => Err(ApiError::new(
            "internal",
            "stream handler did not answer with the subscription it opened",
        )),
    }
}

async fn write_loop(mut sink: FrameSink, outbox: Arc<Outbox>, cancel: CancellationToken) {
    loop {
        let batch = outbox.take_batch(WRITE_BATCH);
        if batch.frames.is_empty() {
            if batch.finished {
                break;
            }
            tokio::select! {
                () = cancel.cancelled() => break,
                () = outbox.writer_wakeup() => continue,
            }
        }
        let write = async {
            for frame in batch.frames {
                sink.feed(frame).await?;
            }
            sink.flush().await
        };
        let written = tokio::select! {
            () = cancel.cancelled() => break,
            written = write => written,
        };
        if let Err(err) = written {
            tracing::debug!(%err, "API connection write failed");
            cancel.cancel();
            // A failed sink is not closed: its error mapper (`SinkMapErr`) runs once and
            // panics if the close fails too (peer gone mid-write).
            return;
        }
        drop(batch.permits);
        outbox.batch_written();
    }
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), sink.close()).await;
}

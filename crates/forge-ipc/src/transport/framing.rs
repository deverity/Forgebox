//! NDJSON framing for byte-stream transports: one frame per line, `\n` terminated.
//!
//! Compact JSON (what `serde_json::to_string` produces) never contains a raw newline, so
//! no escaping is needed. A line longer than the limit is a protocol error: the reading
//! side reports [`TransportError::FrameTooLarge`] once and the stream ends (the server
//! answers with an error and closes the connection). Writers check the limit too, so a
//! peer never receives a frame it would reject. Empty lines are ignored (keep-alive).

use futures::{SinkExt, TryStreamExt, future};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

use super::{Connection, ConnectionInfo, TransportError};

/// Default maximum frame size: 8 MiB (fits `fs.read` of 4 MiB in base64 plus envelope).
pub const DEFAULT_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

fn map_codec_error(err: LinesCodecError, limit: usize) -> TransportError {
    match err {
        LinesCodecError::MaxLineLengthExceeded => TransportError::FrameTooLarge {
            size: limit + 1,
            limit,
        },
        LinesCodecError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            TransportError::InvalidFrame(e.to_string())
        }
        LinesCodecError::Io(e) => match e.kind() {
            std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof => TransportError::Closed,
            _ => TransportError::io("connection I/O", &e),
        },
    }
}

/// Wraps a byte stream into a frame [`Connection`] with frames of at most `max_frame`
/// bytes.
pub fn framed<S>(io: S, info: ConnectionInfo, max_frame: usize) -> Connection
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, write) = tokio::io::split(io);
    let stream = FramedRead::new(read, LinesCodec::new_with_max_length(max_frame))
        .map_err(move |e| map_codec_error(e, max_frame))
        .try_filter(|frame| future::ready(!frame.is_empty()));
    let writer: FramedWrite<_, LinesCodec> = FramedWrite::new(write, LinesCodec::new());
    let sink = SinkExt::<String>::sink_map_err(writer, move |e| map_codec_error(e, max_frame))
        .with(move |frame: String| {
            future::ready(if frame.len() > max_frame {
                Err(TransportError::FrameTooLarge {
                    size: frame.len(),
                    limit: max_frame,
                })
            } else if frame.contains('\n') {
                Err(TransportError::InvalidFrame(
                    "frame contains a raw newline".into(),
                ))
            } else {
                Ok(frame)
            })
        });
    Connection {
        info,
        sink: Box::pin(sink),
        stream: Box::pin(stream),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::transport::TransportKind;

    fn info() -> ConnectionInfo {
        ConnectionInfo {
            transport: TransportKind::InMemory,
            peer: "test".into(),
            local: true,
        }
    }

    #[tokio::test]
    async fn frames_round_trip_and_skip_empty_lines() {
        let (a, b) = tokio::io::duplex(1024);
        let mut left = framed(a, info(), 64);
        let mut right = framed(b, info(), 64);
        left.sink.send(r#"{"a":1}"#.to_owned()).await.unwrap();
        left.sink.send(String::new()).await.unwrap();
        left.sink.send(r#"{"b":2}"#.to_owned()).await.unwrap();
        assert_eq!(right.stream.next().await.unwrap().unwrap(), r#"{"a":1}"#);
        assert_eq!(right.stream.next().await.unwrap().unwrap(), r#"{"b":2}"#);
        drop(left);
        assert!(right.stream.next().await.is_none());
    }

    #[tokio::test]
    async fn oversized_frames_are_refused_on_both_sides() {
        let (a, b) = tokio::io::duplex(4096);
        let mut left = framed(a, info(), 16);
        let err = left.sink.send("x".repeat(17)).await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::FrameTooLarge {
                size: 17,
                limit: 16
            }
        ));

        // A peer with a larger limit sends a frame the reader refuses.
        let mut big = framed(b, info(), 1024);
        big.sink.send("y".repeat(100)).await.unwrap();
        let err = left.stream.next().await.unwrap().unwrap_err();
        assert!(
            matches!(err, TransportError::FrameTooLarge { limit: 16, .. }),
            "{err}"
        );
        assert!(
            left.stream.next().await.is_none(),
            "stream ends after the error"
        );
    }

    #[tokio::test]
    async fn raw_newlines_are_refused() {
        let (a, _b) = tokio::io::duplex(1024);
        let mut left = framed(a, info(), 64);
        let err = left.sink.send("a\nb".to_owned()).await.unwrap_err();
        assert!(matches!(err, TransportError::InvalidFrame(_)));
    }
}

//! The frame codec of `axon/fed/1` (docs/P2P-SPEC.md §8): a `u32` big-endian length, then
//! that many bytes of JSON.

use std::fmt;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame body; a longer announced length is refused before any buffer exists.
pub const MAX_FRAME: usize = 8192;

#[derive(Debug)]
pub enum FrameError {
    /// The announced or outgoing length exceeds `MAX_FRAME`; the receiver closes.
    TooLong(usize),
    Io(std::io::Error),
    NotJson(serde_json::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(len) => write!(f, "frame of {len} bytes exceeds {MAX_FRAME}"),
            Self::Io(err) => write!(f, "frame io: {err}"),
            Self::NotJson(err) => write!(f, "frame is not JSON: {err}"),
        }
    }
}

impl std::error::Error for FrameError {}

pub async fn write_frame(
    out: &mut (impl AsyncWrite + Unpin),
    frame: &Value,
) -> Result<(), FrameError> {
    let body = serde_json::to_vec(frame).map_err(FrameError::NotJson)?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLong(body.len()));
    }
    out.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(FrameError::Io)?;
    out.write_all(&body).await.map_err(FrameError::Io)
}

pub async fn read_frame(input: &mut (impl AsyncRead + Unpin)) -> Result<Value, FrameError> {
    let mut prefix = [0u8; 4];
    input
        .read_exact(&mut prefix)
        .await
        .map_err(FrameError::Io)?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLong(len));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body).await.map_err(FrameError::Io)?;
    serde_json::from_slice(&body).map_err(FrameError::NotJson)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_frame_round_trips() {
        let (mut a, mut b) = tokio::io::duplex(MAX_FRAME * 2);
        let frame = json!({"type": "ping", "v": 1, "generation": 3, "t": 7});
        write_frame(&mut a, &frame).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), frame);
    }

    #[tokio::test]
    async fn the_limit_is_exact() {
        let at_limit = Value::String("x".repeat(MAX_FRAME - 2)); // two quotes make 8192 bytes
        let (mut a, mut b) = tokio::io::duplex(MAX_FRAME * 2);
        write_frame(&mut a, &at_limit).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), at_limit);
        let over = Value::String("x".repeat(MAX_FRAME - 1));
        assert!(matches!(
            write_frame(&mut a, &over).await,
            Err(FrameError::TooLong(8193))
        ));
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_without_reading_the_body() {
        // Only the prefix is on the wire: reading a body would hit EOF, not TooLong.
        let prefix = ((MAX_FRAME + 1) as u32).to_be_bytes();
        let mut input = &prefix[..];
        assert!(matches!(
            read_frame(&mut input).await,
            Err(FrameError::TooLong(8193))
        ));
        let huge = u32::MAX.to_be_bytes();
        assert!(matches!(
            read_frame(&mut &huge[..]).await,
            Err(FrameError::TooLong(_))
        ));
    }

    #[tokio::test]
    async fn truncated_and_non_json_frames_are_errors() {
        let mut short = &[0u8, 0, 0, 5, b'{'][..];
        assert!(matches!(
            read_frame(&mut short).await,
            Err(FrameError::Io(_))
        ));
        let mut junk = &[0u8, 0, 0, 3, b'a', b'b', b'c'][..];
        assert!(matches!(
            read_frame(&mut junk).await,
            Err(FrameError::NotJson(_))
        ));
    }
}

use serde::{de::DeserializeOwned, Serialize};
use std::io::{self, ErrorKind};

use super::packet::{MAGIC, VERSION};

pub const HEADER_SIZE: usize = 15;
pub const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;

pub fn encode_frame<T: Serialize>(
    message_id: u16,
    request_id: u32,
    payload: &T,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let payload = serde_json::to_vec(payload)?;

    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(
            io::Error::new(
                ErrorKind::InvalidData,
                "payload exceeds maximum size",
            )
            .into(),
        );
    }

    let payload_len = u32::try_from(payload.len())
        .map_err(|_| {
            io::Error::new(
                ErrorKind::InvalidData,
                "payload too large",
            )
        })?;

    let mut frame =
        Vec::with_capacity(HEADER_SIZE + payload.len());

    frame.extend_from_slice(&MAGIC);
    frame.push(VERSION);
    frame.extend_from_slice(&message_id.to_be_bytes());
    frame.extend_from_slice(&request_id.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&payload);

    Ok(frame)
}

pub fn decode_frame<T: DeserializeOwned>(
    data: &[u8],
) -> Result<(u16, u32, T), Box<dyn std::error::Error + Send + Sync>> {
    if data.len() < HEADER_SIZE {
        return Err(
            io::Error::new(
                ErrorKind::UnexpectedEof,
                "incomplete packet header",
            )
            .into(),
        );
    }

    if data[0..4] != MAGIC {
        return Err(
            io::Error::new(
                ErrorKind::InvalidData,
                "invalid packet magic",
            )
            .into(),
        );
    }

    if data[4] != VERSION {
        return Err(
            io::Error::new(
                ErrorKind::InvalidData,
                "unsupported protocol version",
            )
            .into(),
        );
    }

    let message_id =
        u16::from_be_bytes([data[5], data[6]]);

    let request_id =
        u32::from_be_bytes([
            data[7],
            data[8],
            data[9],
            data[10],
        ]);

    let payload_len =
        u32::from_be_bytes([
            data[11],
            data[12],
            data[13],
            data[14],
        ]) as usize;

    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(
            io::Error::new(
                ErrorKind::InvalidData,
                "payload exceeds maximum size",
            )
            .into(),
        );
    }

    let expected_len = HEADER_SIZE + payload_len;

    if data.len() < expected_len {
        return Err(
            io::Error::new(
                ErrorKind::UnexpectedEof,
                "incomplete packet payload",
            )
            .into(),
        );
    }

    if data.len() > expected_len {
        return Err(
            io::Error::new(
                ErrorKind::InvalidData,
                "unexpected bytes after packet",
            )
            .into(),
        );
    }

    let payload =
        serde_json::from_slice::<T>(
            &data[HEADER_SIZE..expected_len],
        )?;

    Ok((message_id, request_id, payload))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_preserves_header_and_payload() {
        let payload = serde_json::json!({"type": "Heartbeat", "sequence": 42});
        let encoded = encode_frame(0x0002, 1234, &payload).unwrap();
        let (message_id, request_id, decoded) =
            decode_frame::<serde_json::Value>(&encoded).unwrap();
        assert_eq!(message_id, 0x0002);
        assert_eq!(request_id, 1234);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn decoder_rejects_invalid_magic() {
        let mut frame = encode_frame(1, 1, &serde_json::json!({})).unwrap();
        frame[0] = b'X';
        assert!(decode_frame::<serde_json::Value>(&frame).is_err());
    }

    #[test]
    fn decoder_rejects_truncated_payload() {
        let mut frame = encode_frame(1, 1, &serde_json::json!({"ok": true})).unwrap();
        frame.pop();
        assert!(decode_frame::<serde_json::Value>(&frame).is_err());
    }

    #[test]
    fn decoder_rejects_trailing_bytes() {
        let mut frame = encode_frame(1, 1, &serde_json::json!({})).unwrap();
        frame.push(0);
        assert!(decode_frame::<serde_json::Value>(&frame).is_err());
    }
}

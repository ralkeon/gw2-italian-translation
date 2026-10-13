//! Text Translator module protocol (https://github.com/ideka/modulep, protocol 2).
//!
//! Every message is a little-endian `u32` length followed by that many bytes. The handshake is
//! the only message without a leading `kind` byte. Strings are a `u16` byte length plus UTF-8.
//!
//! The shim forwards the same frames over two named pipes (one per direction, because blocking
//! reads and writes on the same synchronous pipe handle serialize and can deadlock).

use std::io::{self, Read, Write};

pub const PROTOCOL: u32 = 2;
pub const MAX_MESSAGE: usize = 0x10_0000;
pub const CACHE_KEY_MAX: usize = 32;

pub const SOURCE_ENGLISH: i32 = 0;

const KIND_TEXT: u8 = 0;
const KIND_CANCEL: u8 = 1;

/// Pipe the shim writes the addon's requests into (shim -> DLL).
pub fn request_pipe(game_pid: u32) -> String {
    format!(r"\\.\pipe\gw2it-{game_pid}-requests")
}

/// Pipe the DLL writes translations into (DLL -> shim).
pub fn reply_pipe(game_pid: u32) -> String {
    format!(r"\\.\pipe\gw2it-{game_pid}-replies")
}

/// Reads one frame and returns its payload (without the length).
pub fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame too big ({len} bytes)")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(payload)
}

/// Writes one frame (length + payload). Does not flush.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too big"));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)
}

fn put_string(buf: &mut Vec<u8>, s: &str) -> io::Result<()> {
    let len = u16::try_from(s.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "string longer than 65535 bytes"))?;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Handshake payload: must be the first frame the module writes to stdout.
pub fn handshake(source_lang: i32, result_version: u32, cache_key: &str) -> io::Result<Vec<u8>> {
    if cache_key.len() > CACHE_KEY_MAX {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cache key longer than 32 bytes"));
    }
    let mut buf = Vec::with_capacity(14 + cache_key.len());
    buf.extend_from_slice(&PROTOCOL.to_le_bytes());
    buf.extend_from_slice(&source_lang.to_le_bytes());
    buf.extend_from_slice(&result_version.to_le_bytes());
    put_string(&mut buf, cache_key)?;
    Ok(buf)
}

/// A request from the addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddonMessage {
    /// Text to translate: string id, XXH3-64 of the source text, source text.
    Text { id: u32, hash: u64, text: String },
    /// A kind this version does not know; the protocol says to skip it.
    Unknown(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    Truncated,
    InvalidUtf8,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty message",
            Self::Truncated => "truncated message",
            Self::InvalidUtf8 => "invalid UTF-8 string",
        })
    }
}

impl std::error::Error for DecodeError {}

pub fn decode_addon(payload: &[u8]) -> Result<AddonMessage, DecodeError> {
    let (&kind, rest) = payload.split_first().ok_or(DecodeError::Empty)?;
    if kind != KIND_TEXT {
        return Ok(AddonMessage::Unknown(kind));
    }
    let fixed = rest.get(..14).ok_or(DecodeError::Truncated)?;
    let id = u32::from_le_bytes(fixed[0..4].try_into().unwrap());
    let hash = u64::from_le_bytes(fixed[4..12].try_into().unwrap());
    let len = u16::from_le_bytes(fixed[12..14].try_into().unwrap()) as usize;
    let bytes = rest.get(14..14 + len).ok_or(DecodeError::Truncated)?;
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::InvalidUtf8)?;
    Ok(AddonMessage::Text { id, hash, text })
}

/// A translation; `hash` must be the hash the addon sent with the source text.
pub fn encode_text(id: u32, hash: u64, text: &str) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(15 + text.len());
    buf.push(KIND_TEXT);
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&hash.to_le_bytes());
    put_string(&mut buf, text)?;
    Ok(buf)
}

/// Gives up on a request (the addon may ask again later).
pub fn encode_cancel(id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(5);
    buf.push(KIND_CANCEL);
    buf.extend_from_slice(&id.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip() {
        let payload = encode_text(44547, 0xDEAD_BEEF_0123_4567, "Ciao <c=@flavor>mondo</c>").unwrap();
        assert_eq!(
            decode_addon(&payload).unwrap(),
            AddonMessage::Text { id: 44547, hash: 0xDEAD_BEEF_0123_4567, text: "Ciao <c=@flavor>mondo</c>".into() }
        );
    }

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"abc").unwrap();
        assert_eq!(buf, [3, 0, 0, 0, b'a', b'b', b'c']);
        assert_eq!(read_frame(&mut buf.as_slice()).unwrap(), b"abc");
    }

    #[test]
    fn handshake_layout() {
        let hs = handshake(SOURCE_ENGLISH, 7, "it").unwrap();
        assert_eq!(hs, [2, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 2, 0, b'i', b't']);
        assert!(handshake(0, 1, &"x".repeat(33)).is_err());
    }

    #[test]
    fn unknown_and_truncated() {
        assert_eq!(decode_addon(&[9, 1, 2]).unwrap(), AddonMessage::Unknown(9));
        assert_eq!(decode_addon(&[0, 1, 2]), Err(DecodeError::Truncated));
        assert_eq!(decode_addon(&[]), Err(DecodeError::Empty));
    }
}

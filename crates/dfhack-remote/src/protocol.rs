//! Wire framing for the DFHack remote protocol.
//!
//! Layout is taken from `library/include/RemoteClient.h` in DFHack 53.16-r1.1.
//! Both headers are written as raw C structs, so the message header carries two
//! bytes of padding after `id`. Everything is little-endian.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};

pub const REQUEST_MAGIC: &[u8; 8] = b"DFHack?\n";
pub const RESPONSE_MAGIC: &[u8; 8] = b"DFHack!\n";
pub const PROTOCOL_VERSION: i32 = 1;

/// Largest payload the server will accept, from `RPCMessageHeader::MAX_MESSAGE_SIZE`.
pub const MAX_MESSAGE_SIZE: i32 = 64 * 1048576;

/// Method id 0 is always `BindMethod`; id 1 is always `RunCommand`.
pub const ID_BIND_METHOD: i16 = 0;
pub const ID_RUN_COMMAND: i16 = 1;

pub const RPC_REPLY_RESULT: i16 = -1;
pub const RPC_REPLY_FAIL: i16 = -2;
pub const RPC_REPLY_TEXT: i16 = -3;
pub const RPC_REQUEST_QUIT: i16 = -4;

/// `CoreErrorNotification::ErrorCode`, returned in the size field of a failure reply.
pub fn describe_error(code: i32) -> &'static str {
    match code {
        -3 => "CR_LINK_FAILURE",
        -2 => "CR_WOULD_BREAK",
        -1 => "CR_NOT_IMPLEMENTED",
        0 => "CR_OK",
        1 => "CR_FAILURE",
        2 => "CR_WRONG_USAGE",
        3 => "CR_NOT_FOUND",
        _ => "unknown error code",
    }
}

/// The 12-byte handshake request: magic followed by a little-endian version.
pub fn handshake_request() -> [u8; 12] {
    let mut out = [0u8; 12];
    out[..8].copy_from_slice(REQUEST_MAGIC);
    out[8..].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out
}

/// Reads and validates the server's 12-byte handshake reply.
pub fn verify_handshake_reply<S: Read>(stream: &mut S) -> Result<()> {
    let mut reply = [0u8; 12];
    stream
        .read_exact(&mut reply)
        .context("reading handshake reply")?;

    if &reply[..8] != RESPONSE_MAGIC {
        bail!(
            "not a DFHack remote server: expected magic {:?}, got {:?}",
            String::from_utf8_lossy(RESPONSE_MAGIC),
            String::from_utf8_lossy(&reply[..8])
        );
    }
    let version = i32::from_le_bytes(reply[8..].try_into().unwrap());
    if version != PROTOCOL_VERSION {
        bail!("server speaks protocol version {version}, this client speaks {PROTOCOL_VERSION}");
    }
    Ok(())
}

/// Writes an 8-byte message header followed by `payload`.
pub fn write_message<S: Write>(stream: &mut S, id: i16, payload: &[u8]) -> Result<()> {
    let size = i32::try_from(payload.len()).context("payload length overflows i32")?;
    if size > MAX_MESSAGE_SIZE {
        bail!("payload of {size} bytes exceeds the server limit of {MAX_MESSAGE_SIZE}");
    }
    let mut header = [0u8; 8];
    header[..2].copy_from_slice(&id.to_le_bytes());
    // bytes 2..4 are struct padding and stay zero
    header[4..].copy_from_slice(&size.to_le_bytes());

    stream.write_all(&header)?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

/// Reads one 8-byte message header, returning `(id, size)`.
///
/// A `RPC_REPLY_FAIL` header carries the error code in `size` and has no payload.
pub fn read_header<S: Read>(stream: &mut S) -> Result<(i16, i32)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).context("reading header")?;
    let id = i16::from_le_bytes(header[..2].try_into().unwrap());
    let size = i32::from_le_bytes(header[4..].try_into().unwrap());
    Ok((id, size))
}

/// Reads a payload of exactly `size` bytes.
pub fn read_payload<S: Read>(stream: &mut S, size: i32) -> Result<Vec<u8>> {
    if !(0..=MAX_MESSAGE_SIZE).contains(&size) {
        bail!("server announced an implausible payload size of {size} bytes");
    }
    let mut buf = vec![0u8; size as usize];
    stream.read_exact(&mut buf).context("reading payload")?;
    Ok(buf)
}

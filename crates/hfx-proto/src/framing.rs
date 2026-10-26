// SPDX-License-Identifier: GPL-2.0-only

//! Message framing: a big-endian `u32` payload length, then that many bytes
//! of UTF-8 JSON. Lengths of 0 or above [`MAX_MESSAGE_BYTES`] are rejected
//! before anything is allocated.

use std::fmt;
use std::io::{self, Read, Write};

pub const MAX_MESSAGE_BYTES: usize = 1_048_576;
pub const LENGTH_PREFIX_BYTES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    Io(io::ErrorKind),
    /// The stream ended inside the length prefix.
    TruncatedLength,
    EmptyPayload,
    TooLarge(usize),
    /// The stream ended inside the payload.
    TruncatedPayload,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "frame I/O failed: {kind}"),
            Self::TruncatedLength => f.write_str("stream ended inside a frame length"),
            Self::EmptyPayload => f.write_str("frame declares an empty payload"),
            Self::TooLarge(n) => write!(f, "frame declares {n} bytes, above {MAX_MESSAGE_BYTES}"),
            Self::TruncatedPayload => f.write_str("stream ended inside a frame payload"),
        }
    }
}

impl std::error::Error for FrameError {}

fn check_length(length: usize) -> Result<(), FrameError> {
    match length {
        0 => Err(FrameError::EmptyPayload),
        n if n > MAX_MESSAGE_BYTES => Err(FrameError::TooLarge(n)),
        _ => Ok(()),
    }
}

/// Length prefix and payload in one buffer. A writer never emits half a
/// frame because the second write failed.
///
/// # Errors
/// Fails for an empty or oversized payload.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    check_length(payload.len())?;
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(payload.len()))?;
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// # Errors
/// Fails for an invalid payload size or a write error.
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    let frame = encode_frame(payload)?;
    writer
        .write_all(&frame)
        .map_err(|e| FrameError::Io(e.kind()))?;
    writer.flush().map_err(|e| FrameError::Io(e.kind()))
}

fn read_fully<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<usize, FrameError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e.kind())),
        }
    }
    Ok(filled)
}

/// Blocking read of one frame. `Ok(None)` is a clean end of stream before
/// any byte of a new frame.
///
/// # Errors
/// Fails for I/O errors, truncation and invalid lengths.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
    match read_fully(reader, &mut prefix)? {
        0 => return Ok(None),
        LENGTH_PREFIX_BYTES => {}
        _ => return Err(FrameError::TruncatedLength),
    }
    let length = u32::from_be_bytes(prefix) as usize;
    check_length(length)?;
    let mut payload = vec![0_u8; length];
    if read_fully(reader, &mut payload)? != length {
        return Err(FrameError::TruncatedPayload);
    }
    Ok(Some(payload))
}

/// Incremental frame decoder for non-blocking sockets: feed it whatever
/// `read()` returned, then take complete frames out.
///
/// A bad length is reported as soon as its four bytes arrive, and the
/// buffer never holds more than one maximum-size frame plus one prefix, so
/// a client cannot make the bridge buffer unbounded data.
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    pub const MAX_BUFFERED: usize = MAX_MESSAGE_BYTES + 2 * LENGTH_PREFIX_BYTES;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many bytes the caller may read before the next [`Self::push`]
    /// without exceeding the buffer bound.
    #[must_use]
    pub fn room(&self) -> usize {
        Self::MAX_BUFFERED.saturating_sub(self.buffer.len())
    }

    /// # Errors
    /// Fails when the bytes would overflow the buffer bound. The connection
    /// should be closed.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        if bytes.len() > self.room() {
            return Err(FrameError::TooLarge(self.buffer.len() + bytes.len()));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// The next complete payload, if one is buffered.
    ///
    /// # Errors
    /// Fails for an invalid length prefix.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let Some(prefix) = self.buffer.first_chunk::<LENGTH_PREFIX_BYTES>() else {
            return Ok(None);
        };
        let length = u32::from_be_bytes(*prefix) as usize;
        check_length(length)?;
        let end = LENGTH_PREFIX_BYTES + length;
        if self.buffer.len() < end {
            return Ok(None);
        }
        let payload = self.buffer[LENGTH_PREFIX_BYTES..end].to_vec();
        self.buffer.drain(..end);
        Ok(Some(payload))
    }

    /// Call at end of stream: buffered bytes mean the peer quit mid-frame.
    ///
    /// # Errors
    /// Fails when a partial frame is buffered.
    pub fn finish(&self) -> Result<(), FrameError> {
        match self.buffer.len() {
            0 => Ok(()),
            n if n < LENGTH_PREFIX_BYTES => Err(FrameError::TruncatedLength),
            _ => Err(FrameError::TruncatedPayload),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip() {
        let frame = encode_frame(b"{}").unwrap();
        assert_eq!(frame, [0, 0, 0, 2, b'{', b'}']);
        let mut cursor = Cursor::new(frame);
        assert_eq!(read_frame(&mut cursor).unwrap().unwrap(), b"{}");
        assert_eq!(read_frame(&mut cursor).unwrap(), None);
    }

    #[test]
    fn bad_lengths() {
        assert_eq!(encode_frame(b""), Err(FrameError::EmptyPayload));
        let too_big = ((MAX_MESSAGE_BYTES + 1) as u32).to_be_bytes();
        assert_eq!(
            read_frame(&mut Cursor::new(too_big)),
            Err(FrameError::TooLarge(MAX_MESSAGE_BYTES + 1))
        );
        assert_eq!(
            read_frame(&mut Cursor::new([0, 0, 0, 0])),
            Err(FrameError::EmptyPayload)
        );
        assert_eq!(
            read_frame(&mut Cursor::new([0, 0])),
            Err(FrameError::TruncatedLength)
        );
        assert_eq!(
            read_frame(&mut Cursor::new([0, 0, 0, 3, b'{'])),
            Err(FrameError::TruncatedPayload)
        );
    }

    #[test]
    fn incremental_reader() {
        let mut stream = encode_frame(b"one").unwrap();
        stream.extend(encode_frame(b"two").unwrap());
        let mut reader = FrameReader::new();
        let mut frames = Vec::new();
        for byte in stream {
            reader.push(&[byte]).unwrap();
            while let Some(frame) = reader.next_frame().unwrap() {
                frames.push(frame);
            }
        }
        assert_eq!(frames, [b"one".to_vec(), b"two".to_vec()]);
        reader.finish().unwrap();

        let mut reader = FrameReader::new();
        reader.push(&[0xff, 0xff, 0xff, 0xff]).unwrap();
        assert!(matches!(reader.next_frame(), Err(FrameError::TooLarge(_))));

        let mut reader = FrameReader::new();
        reader.push(&[0, 0, 0, 5, b'a']).unwrap();
        assert_eq!(reader.next_frame().unwrap(), None);
        assert_eq!(reader.finish(), Err(FrameError::TruncatedPayload));
    }
}

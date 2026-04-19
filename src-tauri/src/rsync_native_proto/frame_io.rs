//! Length-prefixed stdio framing used by the live RSNP prototype transport.
//!
//! This is deliberately separate from the RSNP envelope in `protocol.rs`.
//! The RSNP envelope defines message content; this module defines how multiple
//! opaque frames are multiplexed over one byte stream.

use std::io::{self, Read, Write};

pub fn write_length_prefixed_frame<W: Write>(writer: &mut W, frame: &[u8]) -> io::Result<()> {
    let len = u32::try_from(frame.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(frame)?;
    writer.flush()?;
    Ok(())
}

pub fn read_length_prefixed_frame<R: Read>(
    reader: &mut R,
    max_frame_size: usize,
) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > max_frame_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {} exceeds max {}", len, max_frame_size),
        ));
    }
    let mut out = vec![0u8; len];
    reader.read_exact(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{read_length_prefixed_frame, write_length_prefixed_frame};
    use std::io::Cursor;

    #[test]
    fn round_trip_small_frame() {
        let mut buffer = Vec::new();
        write_length_prefixed_frame(&mut buffer, b"hello").unwrap();
        let mut cursor = Cursor::new(buffer);
        let frame = read_length_prefixed_frame(&mut cursor, 1024).unwrap();
        assert_eq!(frame, b"hello");
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(4097u32).to_be_bytes());
        raw.resize(4 + 4097, 0);
        let mut cursor = Cursor::new(raw);
        let err = read_length_prefixed_frame(&mut cursor, 4096).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}

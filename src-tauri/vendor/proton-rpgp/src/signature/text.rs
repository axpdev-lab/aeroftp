use std::{
    cell::RefCell,
    io::{self, Read},
    rc::Rc,
};

use nom::Input;
use pgp::{line_writer::LineBreak, normalize_lines::NormalizedReader};
use sha2::digest::DynDigest;

use crate::TextSanitizationError;

/// Checks that the input is utf-8 encoded and
/// replaces canonical newlines (`\r\n`) with native newlines (`\n`).
pub(crate) fn check_and_sanitize_text(cleartext: &[u8]) -> Result<Vec<u8>, TextSanitizationError> {
    let mut buffer = String::with_capacity(cleartext.len());
    NormalizedReader::new(cleartext, LineBreak::Lf).read_to_string(&mut buffer)?;
    Ok(buffer.into_bytes())
}

/// Copied code from [`pgp::normalize_lines::NormalizedReader`]
/// in rPGP as constructors and fields are private.
pub struct NormalizingHasher {
    hasher: Box<dyn DynDigest + Send>,
    text_mode: bool,
    last_was_cr: bool,
}

impl NormalizingHasher {
    pub(crate) fn new(hasher: Box<dyn DynDigest + Send>, text_mode: bool) -> Self {
        Self {
            hasher,
            text_mode,
            last_was_cr: false,
        }
    }

    pub(crate) fn done(mut self) -> Box<dyn DynDigest + Send> {
        if self.text_mode && self.last_was_cr {
            self.hasher.update(b"\n");
        }

        self.hasher
    }

    #[allow(clippy::indexing_slicing)]
    pub(crate) fn hash_buf(&mut self, buffer: &[u8]) {
        if buffer.is_empty() {
            return;
        }

        if self.text_mode {
            let mut buf = buffer;

            if self.last_was_cr {
                // detect and handle a LF that follows a CR
                // (it should not be normalized)
                if buf[0] == b'\n' {
                    self.hasher.update(b"\n");
                    buf = &buf[1..];
                }

                self.last_was_cr = false;
            }

            while !buf.is_empty() {
                match buf.position(|c| c == b'\r' || c == b'\n') {
                    None => {
                        // no line endings in sight, just hash the data
                        self.hasher.update(buf);
                        buf = &[];
                    }

                    Some(pos) => {
                        // consume all bytes before line-break-related position

                        self.hasher.update(&buf[..pos]);
                        buf = &buf[pos..];

                        // handle this line-break related context
                        let only_one = buf.len() == 1;
                        match (buf[0], only_one) {
                            (b'\n', _) => {
                                self.hasher.update(b"\r\n");
                                buf = &buf[1..];
                            }
                            (b'\r', false) => {
                                // we are guaranteed to have at least two bytes
                                if buf[1] == b'\n' {
                                    // there was a '\n' in the stream, we consume it as well
                                    self.hasher.update(b"\r\n");
                                    buf = &buf[2..];
                                } else {
                                    // this was a lone '\r', we don't normalize it
                                    self.hasher.update(b"\r");
                                    buf = &buf[1..];
                                }
                            }
                            (b'\r', true) => {
                                // this one '\r' was the last thing in the buffer
                                self.hasher.update(b"\r");
                                buf = &[];

                                // remember that the last character was a CR.
                                // if the next character is a LF, we want to *not* normalize it
                                self.last_was_cr = true;
                            }
                            _ => unreachable!("buf.position gave us either a '\n or a '\r'"),
                        }
                    }
                }
            }
        } else {
            self.hasher.update(buffer);
        }
    }
}

pub type ReaderReference<R> = Rc<RefCell<R>>;

pub struct ReferencedReader<R>
where
    R: Read,
{
    inner: Rc<RefCell<R>>,
}

impl<R: Read> ReferencedReader<R> {
    pub(crate) fn new(source: R) -> Self {
        Self {
            inner: Rc::new(RefCell::new(source)),
        }
    }

    pub(crate) fn reference(&self) -> ReaderReference<R> {
        Rc::clone(&self.inner)
    }
}

impl<R: Read> Read for ReferencedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.borrow_mut().read(buf)
    }
}

/// Wrapping reader that checks that the input data is valid UTF-8.
///
/// Non-UTF-8 data in the input stream is rejected with an `io::Error`.
/// COPIED FROM rPGP
pub(crate) struct Utf8CheckReader<R>
where
    R: Read,
{
    source: R,

    // Overhang bytes from the last read, if any.
    // If this is `Some`, it contains bytes that we'll prepend and check with the next read.
    rest: Option<Vec<u8>>,
}

impl<R: Read> Utf8CheckReader<R> {
    pub(crate) fn new(source: R) -> Self {
        Self { source, rest: None }
    }
}

impl<R: Read> Read for Utf8CheckReader<R> {
    #[allow(clippy::indexing_slicing)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Checks if `data` contains valid utf-8 and returns up to 3 bytes of overhang, which
        // might add up to a valid codepoint with more data in the following read.
        // Errors if `data` is definitely not UTF-8.
        fn check_utf8(data: &[u8]) -> Result<Option<Vec<u8>>, io::Error> {
            match std::str::from_utf8(data) {
                Ok(_) => Ok(None),
                Err(err) => {
                    let valid_up_to = err.valid_up_to();

                    // handle the remaining data, which may be a fragment of UTF-8 that will be
                    // completed in the next read
                    let rest = data
                        .get(valid_up_to..)
                        .ok_or(io::Error::other("Slice is out of bounds"))?;

                    match rest.len() {
                        0 => Ok(None),
                        1..=3 => Ok(Some(Vec::from(rest))),

                        // 3 bytes is the longest possibly legal intermediate fragment of UTF-8 data.
                        // If `rest` is longer, then the data is definitely not valid UTF-8.
                        4.. => Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid UTF-8 data",
                        )),
                    }
                }
            }
        }

        let len = self.source.read(buf)?;

        if len == 0 {
            // We reached the end of the input stream

            // If the UTF-8 parsing seems to be stuck mid-codepoint, we error
            if self.rest.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid UTF-8 data",
                ));
            }

            return Ok(0);
        }

        self.rest = if let Some(mut check) = self.rest.take() {
            // check overhang from last read + the new data from this read
            check.extend_from_slice(&buf[..len]);
            check_utf8(&check)?
        } else {
            // we have no overhang from the last read, just check the data from this read
            check_utf8(&buf[..len])?
        };

        Ok(len)
    }
}

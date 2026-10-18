use std::{
    fmt,
    io::{self, BufRead, Read},
};

use pgp::{
    bytes::{Buf, BufMut, BytesMut},
    composed::Message,
    normalize_lines::NormalizedReader,
    packet::{Signature, SignatureType, SignatureVersionSpecific},
};

use crate::{
    MessageSignatureError, MessageVerificationExt, NormalizingHasher, ReaderReference,
    ReferencedReader, SignatureError, SignatureVerificationResult, VerificationError,
    VerificationInput, VerificationResult, VerificationResultCreator, Verifier,
};

const BUFFER_SIZE: usize = 8 * 1024;

pub enum VerifyingReader<'a> {
    InlineNormalizedLineEndings {
        referenced_inner_reader: ReaderReference<LimitingReader<MessageVerifyingReader<'a>>>,
        normalized_reader:
            Box<NormalizedReader<ReferencedReader<LimitingReader<MessageVerifyingReader<'a>>>>>,
    },
    Inline(LimitingReader<MessageVerifyingReader<'a>>),
    Detached(DetachedVerifyingReader<'a>),
}

impl VerifyingReader<'_> {
    pub fn discard_all_data(&mut self) -> io::Result<()> {
        io::copy(self, &mut io::sink()).map(|_| ())
    }
}

impl Read for VerifyingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::InlineNormalizedLineEndings {
                normalized_reader, ..
            } => normalized_reader.read(buf),
            Self::Inline(reader) => reader.read(buf),
            Self::Detached(reader) => reader.read(buf),
        }
    }
}

impl fmt::Debug for VerifyingReader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InlineNormalizedLineEndings { .. } => {
                f.write_str("inline signature verification reader with normalized line endings")
            }
            Self::Inline(_) => f.write_str("inline signature verification reader"),
            Self::Detached(_) => f.write_str("detached signature verification reader"),
        }
    }
}

impl VerifyingReader<'_> {
    pub fn verification_result(self) -> VerificationResult {
        match self {
            Self::InlineNormalizedLineEndings {
                referenced_inner_reader: inner_referenced_reader,
                ..
            } => inner_referenced_reader
                .borrow()
                .as_inner()
                .verification_result(),
            Self::Inline(reader) => reader.into_inner().verification_result(),
            Self::Detached(reader) => reader.verification_result(),
        }
    }
}

impl<'a> From<LimitingReader<MessageVerifyingReader<'a>>> for VerifyingReader<'a> {
    fn from(reader: LimitingReader<MessageVerifyingReader<'a>>) -> Self {
        Self::Inline(reader)
    }
}

impl<'a> From<DetachedVerifyingReader<'a>> for VerifyingReader<'a> {
    fn from(reader: DetachedVerifyingReader<'a>) -> Self {
        Self::Detached(reader)
    }
}

pub struct MessageVerifyingReader<'a> {
    verifier: Verifier<'a>,
    message: Box<Message<'a>>,
}

impl<'a> MessageVerifyingReader<'a> {
    pub(crate) fn new(verifier: Verifier<'a>, message: Message<'a>) -> Self {
        Self {
            verifier,
            message: Box::new(message),
        }
    }

    pub fn verification_result(&self) -> VerificationResult {
        let verified_signatures = self
            .message
            .verify_message_signatures(
                self.verifier.date,
                &self.verifier.verification_keys,
                self.verifier.verification_context.as_deref(),
                &self.verifier.profile,
            )
            .map_err(|err| VerificationError::RuntimeError(err.to_string()))?;

        VerificationResultCreator::with_signatures(verified_signatures)
    }
}

impl Read for MessageVerifyingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.message.read(buf)
    }
}

pub struct DetachedVerifyingReader<'a> {
    verifier: Verifier<'a>,
    reader: DetachedSignaturesBodyReader<'a>,
}

impl<'a> DetachedVerifyingReader<'a> {
    pub(crate) fn new(
        verifier: Verifier<'a>,
        sig: impl IntoIterator<Item = Signature>,
        source: Box<dyn BufRead + 'a>,
    ) -> Self {
        Self {
            verifier,
            reader: DetachedSignaturesBodyReader::new(sig, source),
        }
    }

    pub fn verification_result(self) -> VerificationResult {
        let verified_signatures = self
            .reader
            .into_hashes()
            .into_iter()
            .map(|(result_hash, signature)| match result_hash {
                Ok(hash) => SignatureVerificationResult::create_by_verifying(
                    self.verifier.date,
                    signature,
                    &self.verifier.verification_keys,
                    VerificationInput::Hash(&hash),
                    self.verifier.verification_context.as_deref(),
                    &self.verifier.profile,
                ),
                Err(err) => SignatureVerificationResult {
                    signature,
                    verified_by: None,
                    verification_result: Err(MessageSignatureError::Failed(
                        SignatureError::HashComputation(err),
                    )),
                },
            })
            .collect();

        VerificationResultCreator::with_signatures(verified_signatures)
    }
}

impl Read for DetachedVerifyingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

type HashResult = Result<Box<[u8]>, pgp::errors::Error>;

/// Low level reader to compute the hash for detached signatures.
/// Similar to the [`pgp::composed::SignatureBodyReader`] in rPGP.
pub enum DetachedSignaturesBodyReader<'a> {
    Init {
        source: Box<dyn BufRead + 'a>,
        norm_hashers: Vec<(Result<NormalizingHasher, pgp::errors::Error>, Signature)>,
    },
    Body {
        source: Box<dyn BufRead + 'a>,
        buffer: BytesMut,
        norm_hashers: Vec<(Result<NormalizingHasher, pgp::errors::Error>, Signature)>,
    },
    Done {
        source: Box<dyn BufRead + 'a>,
        hashes: Vec<(HashResult, Signature)>,
    },
    Error,
}

impl<'a> DetachedSignaturesBodyReader<'a> {
    pub(crate) fn new(
        sig: impl IntoIterator<Item = Signature>,
        source: Box<dyn BufRead + 'a>,
    ) -> Self {
        let norm_hashers = sig
            .into_iter()
            .map(|sig| {
                let hasher = sig
                    .config()
                    .ok_or_else(|| pgp::errors::Error::Message {
                        message: "No signature config".into(),
                        backtrace: None,
                    })
                    .and_then(|config| {
                        let mut hasher = config.hash_alg.new_hasher()?;
                        if let SignatureVersionSpecific::V6 { ref salt, .. } =
                            config.version_specific
                        {
                            hasher.update(salt.as_ref());
                        }
                        Ok(hasher)
                    });
                let text_mode = sig.typ() == Some(SignatureType::Text);
                let norm_hasher = hasher.map(|h| NormalizingHasher::new(h, text_mode));
                (norm_hasher, sig)
            })
            .collect();

        Self::Init {
            source,
            norm_hashers,
        }
    }

    fn into_hashes(self) -> Vec<(HashResult, Signature)> {
        match self {
            Self::Init { norm_hashers, .. } => norm_hashers
                .into_iter()
                .map(|(_, signature)| {
                    (
                        Err(pgp::errors::Error::Message {
                            message: "Data has not been hashed yet".into(),
                            backtrace: None,
                        }),
                        signature,
                    )
                })
                .collect(),
            Self::Body { norm_hashers, .. } => norm_hashers
                .into_iter()
                .map(|(_, signature)| {
                    (
                        Err(pgp::errors::Error::Message {
                            message: "No completely read".into(),
                            backtrace: None,
                        }),
                        signature,
                    )
                })
                .collect(),
            Self::Done { hashes, .. } => hashes,
            Self::Error => Vec::new(),
        }
    }

    fn fill_inner(&mut self) -> io::Result<()> {
        if matches!(self, Self::Done { .. }) {
            return Ok(());
        }

        loop {
            match std::mem::replace(self, Self::Error) {
                Self::Init {
                    mut norm_hashers,
                    mut source,
                } => {
                    let mut buffer = BytesMut::with_capacity(BUFFER_SIZE);
                    fill_buffer_bytes(&mut source, &mut buffer, BUFFER_SIZE)?;

                    for (norm_hasher, _) in &mut norm_hashers {
                        if let Ok(ref mut hasher) = norm_hasher {
                            hasher.hash_buf(&buffer);
                        }
                    }

                    *self = Self::Body {
                        source,
                        norm_hashers,
                        buffer,
                    };
                }
                Self::Body {
                    mut norm_hashers,
                    mut source,
                    mut buffer,
                } => {
                    if buffer.has_remaining() {
                        *self = Self::Body {
                            norm_hashers,
                            source,
                            buffer,
                        };
                        return Ok(());
                    }

                    let read = fill_buffer_bytes(&mut source, &mut buffer, BUFFER_SIZE)?;

                    for (norm_hasher, _) in &mut norm_hashers {
                        if let Ok(ref mut hasher) = norm_hasher {
                            hasher.hash_buf(&buffer);
                        }
                    }

                    if read == 0 {
                        *self = Self::Done {
                            source,
                            hashes: Self::finalize_hashes(norm_hashers),
                        }
                    } else {
                        *self = Self::Body {
                            norm_hashers,
                            source,
                            buffer,
                        }
                    }

                    return Ok(());
                }
                Self::Done { hashes, source } => {
                    *self = Self::Done { hashes, source };
                    return Ok(());
                }
                Self::Error => return Err(io::Error::other("SignatureBodyReader errored")),
            }
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done { .. })
    }

    fn finalize_hashes(
        norm_hashers: Vec<(Result<NormalizingHasher, pgp::errors::Error>, Signature)>,
    ) -> Vec<(HashResult, Signature)> {
        norm_hashers
            .into_iter()
            .map(|(norm_hasher, signature)| {
                let hash_result = norm_hasher.and_then(|norm_hasher| {
                    let mut raw_hasher = norm_hasher.done();
                    let config = signature
                        .config()
                        .ok_or_else(|| pgp::errors::Error::Message {
                            message: "No signature config found".into(),
                            backtrace: None,
                        })?;
                    let len = config.hash_signature_data(&mut raw_hasher)?;
                    let trailer = config.trailer(len)?;
                    raw_hasher.update(&trailer);
                    Ok(raw_hasher.finalize())
                });
                (hash_result, signature)
            })
            .collect()
    }
}

impl BufRead for DetachedSignaturesBodyReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.fill_inner()?;
        match self {
            Self::Init { .. } => unreachable!("invalid state"),
            Self::Body { buffer, .. } => Ok(&buffer[..]),
            Self::Done { .. } => Ok(&[][..]),
            Self::Error => Err(io::Error::other("SignatureBodyReader errored")),
        }
    }

    fn consume(&mut self, amt: usize) {
        match self {
            Self::Init { .. } => panic!("must not be called before fill_buf"),
            Self::Body { buffer, .. } => {
                buffer.advance(amt);
            }
            Self::Done { .. } | Self::Error => {}
        }
    }
}

impl Read for DetachedSignaturesBodyReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.fill_inner()?;
        match self {
            Self::Init { .. } => unreachable!("invalid state"),
            Self::Body { buffer, .. } => {
                let to_write = buffer.remaining().min(buf.len());
                let to_write_slice = buf
                    .get_mut(..to_write)
                    .ok_or(io::Error::other("Slice is out of bounds"))?;
                buffer.copy_to_slice(to_write_slice);
                Ok(to_write)
            }
            Self::Done { .. } => Ok(0),
            Self::Error => Err(io::Error::other("SignatureBodyReader errored")),
        }
    }
}

/// Copied code from rPGP for [`DetachedSignaturesBodyReader`].
pub(crate) fn fill_buffer_bytes<R: BufRead>(
    mut source: R,
    buffer: &mut BytesMut,
    len: usize,
) -> io::Result<usize> {
    let mut read_total = 0;
    while buffer.remaining() < len {
        let source_buffer = source.fill_buf()?;
        let read = source_buffer.len().min(len - buffer.remaining());
        let to_read = source_buffer
            .get(..read)
            .ok_or(io::Error::other("Slice is out of bounds"))?;
        buffer.put_slice(to_read);
        read_total += read;
        source.consume(read);

        if read == 0 {
            break;
        }
    }
    Ok(read_total)
}

pub struct LimitingReader<R: Read> {
    inner: R,
    bytes_read: usize,
    limit: Option<usize>,
}

impl<R: Read> LimitingReader<R> {
    pub(crate) fn new(inner: R, limit: Option<usize>) -> Self {
        Self {
            inner,
            bytes_read: 0,
            limit,
        }
    }

    pub(crate) fn into_inner(self) -> R {
        self.inner
    }

    pub(crate) fn as_inner(&self) -> &R {
        &self.inner
    }
}

impl<R: Read> Read for LimitingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.bytes_read += bytes_read;
        if let Some(limit) = self.limit {
            if self.bytes_read > limit {
                return Err(io::Error::other(format!("limit of {limit} bytes exceeded")));
            }
        }
        Ok(bytes_read)
    }
}

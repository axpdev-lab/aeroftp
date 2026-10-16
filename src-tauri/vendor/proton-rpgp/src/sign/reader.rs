use std::{
    cell::RefCell,
    io::{self, Read, Write},
    rc::Rc,
};

use pgp::{composed::DetachedSignature, packet::SignatureHasher, types::Password};

use crate::{sign::handle_signature_encoding, DataEncoding, Signer, SigningError};

/// Generates detached signatures via a source reader.
///
/// Once all data has been read, the detached signatures can created by calling [`DetachedSignatureGenerator::finalize`].
pub struct DetachedSignatureGenerator<'a> {
    signer: Signer<'a>,
    encoding: DataEncoding,
    inner: InnerDetachedHashingReader<'a>,
}

impl<'a> DetachedSignatureGenerator<'a> {
    pub(crate) fn new(
        signer: Signer<'a>,
        inner: InnerDetachedHashingReader<'a>,
        encoding: DataEncoding,
    ) -> Self {
        Self {
            signer,
            encoding,
            inner,
        }
    }

    /// Reads all data from the source and discards it.
    pub fn discard_all_data(&mut self) -> io::Result<()> {
        io::copy(self, &mut io::sink()).map(|_| ())
    }

    /// Finalizes the signature generation and returns the signatures.
    ///
    /// The reader must have been fully read to produce valid signatures.
    pub fn finalize(self) -> Result<Vec<u8>, SigningError> {
        self.inner
            .external_hash_tracker()
            .sign_with(&self.signer, self.encoding)
    }
}

impl Read for DetachedSignatureGenerator<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

#[derive(Default)]
enum HashState {
    Hashing(Vec<SignatureHasher>),
    Sign(Vec<SignatureHasher>),
    #[default]
    Empty,
}

/// External handle to the hash computation state in [`InnerDetachedHashingReader`].
///
/// Enables creation of detached signatures after reading is complete.
/// Such a type is required because the Encryptor passes the source reader to encryption,
/// but still might have to create a detached signature once all data has been read.
/// This tracker allows to share ownership of the hash computation state with the Encryptor.
#[derive(Clone)]
pub(crate) struct ExternalHashTracker(Rc<RefCell<HashState>>);

impl ExternalHashTracker {
    pub(crate) fn new(signatures: Vec<SignatureHasher>) -> Self {
        Self(Rc::new(RefCell::new(HashState::Hashing(signatures))))
    }

    pub(crate) fn sign_with(
        self,
        signer: &Signer<'_>,
        data_encoding: DataEncoding,
    ) -> Result<Vec<u8>, SigningError> {
        let HashState::Sign(signatures) = self.0.take() else {
            return Err(SigningError::NotAllDataRead);
        };

        let signing_keys = signer
            .select_signing_keys()
            .map_err(SigningError::KeySelection)?;

        let signatures_result: Result<Vec<_>, SigningError> = signatures
            .into_iter()
            .zip(signing_keys)
            .map(|(signature, key)| {
                signature
                    .sign(&key.private_key, &Password::empty())
                    .map(DetachedSignature::new)
                    .map_err(SigningError::Sign)
            })
            .collect();

        let signatures = signatures_result?;
        let signature_bytes =
            handle_signature_encoding(&signatures, data_encoding.resolve_for_write())?;

        Ok(signature_bytes)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match &mut *self.0.borrow_mut() {
            HashState::Hashing(ref mut signatures) => {
                for signature in signatures {
                    signature.write_all(buf)?;
                }
                Ok(())
            }
            HashState::Sign(_) => Ok(()),
            HashState::Empty => Err(io::Error::other("No hashes to write")),
        }
    }
}

/// Reader that produces a hash for each detached signature.
///
/// Can be passed to other readers and verifed via the [`ExternalHashTracker`].
pub(crate) struct InnerDetachedHashingReader<'a> {
    source: Box<dyn Read + 'a>,
    hash_tracker: ExternalHashTracker,
}

impl<'a> InnerDetachedHashingReader<'a> {
    pub(crate) fn new(source: Box<dyn Read + 'a>, signatures: Vec<SignatureHasher>) -> Self {
        Self {
            source,
            hash_tracker: ExternalHashTracker::new(signatures),
        }
    }

    pub(crate) fn external_hash_tracker(&self) -> ExternalHashTracker {
        self.hash_tracker.clone()
    }
}

impl Read for InnerDetachedHashingReader<'_> {
    #[allow(clippy::indexing_slicing)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (bytes_read, hashing) = match &mut *self.hash_tracker.0.borrow_mut() {
            HashState::Hashing { .. } => {
                let read = self.source.read(buf)?;
                Ok((read, true))
            }
            HashState::Sign { .. } => Ok((0, false)),
            HashState::Empty => Err(io::Error::other("No hashes to write")),
        }?;
        if hashing && bytes_read == 0 && !buf.is_empty() {
            if let HashState::Hashing(signatures) = self.hash_tracker.0.take() {
                self.hash_tracker.0.replace(HashState::Sign(signatures));
            }
            return Ok(0);
        }
        self.hash_tracker.write_all(&buf[..bytes_read])?;
        Ok(bytes_read)
    }
}

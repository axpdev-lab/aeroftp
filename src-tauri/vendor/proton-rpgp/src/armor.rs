use std::io::{self, BufRead, Read};

use pgp::{
    armor::{self, BlockType, Dearmor},
    composed::Message,
    ser::Serialize,
};

use crate::{ArmorError, MessageProcessingError, ResolvedDataEncoding};

const INCLUDE_CHECKSUM: bool = true;

/// Armors a public key.
///
/// Produces data of the form:
/// ```skip
/// -----BEGIN PGP PUBLIC KEY BLOCK-----
///
/// ...
/// -----END PGP PUBLIC KEY BLOCK-----
/// ```
pub fn armor_public_key(public_key: impl AsRef<[u8]>) -> crate::Result<Vec<u8>> {
    let mut armored = Vec::with_capacity(public_key.as_ref().len());
    encode_to_buffer(
        &BinaryArmorSource {
            data: public_key.as_ref(),
        },
        BlockType::PublicKey,
        INCLUDE_CHECKSUM,
        &mut armored,
    )?;
    Ok(armored)
}

/// Armors a private key.
///
/// Produces data of the form:
/// ```skip
/// -----BEGIN PGP <TYPE> KEY BLOCK-----
///
/// ...
/// -----END PGP <TYPE> KEY BLOCK-----
/// where <TYPE> `PRIVATE`.
/// ```
pub fn armor_private_key(private_key: impl AsRef<[u8]>) -> crate::Result<Vec<u8>> {
    let mut armored = Vec::with_capacity(private_key.as_ref().len());
    encode_to_buffer(
        &BinaryArmorSource {
            data: private_key.as_ref(),
        },
        BlockType::PrivateKey,
        INCLUDE_CHECKSUM,
        &mut armored,
    )?;
    Ok(armored)
}

/// Armors a signature.
///
/// Produces data of the form:
/// ```skip
/// -----BEGIN PGP SIGNATURE-----
///
/// ...
/// -----END PGP SIGNATURE-----
/// ```
pub fn armor_signature(signature: impl AsRef<[u8]>) -> crate::Result<Vec<u8>> {
    let mut armored = Vec::with_capacity(signature.as_ref().len());
    encode_to_buffer(
        &BinaryArmorSource {
            data: signature.as_ref(),
        },
        BlockType::Signature,
        INCLUDE_CHECKSUM,
        &mut armored,
    )?;
    Ok(armored)
}

/// Armors a message.
///
/// Produces data of the form:
/// ```skip
/// -----BEGIN PGP MESSAGE-----
///
/// ...
/// -----END PGP MESSAGE-----
/// ```
pub fn armor_message(message: impl AsRef<[u8]>) -> crate::Result<Vec<u8>> {
    let mut armored = Vec::with_capacity(message.as_ref().len());
    encode_to_buffer(
        &BinaryArmorSource {
            data: message.as_ref(),
        },
        BlockType::Message,
        INCLUDE_CHECKSUM,
        &mut armored,
    )?;
    Ok(armored)
}

/// Unarmors the input.
///
/// The input must have the form:
/// ```skip
/// -----BEGIN PGP <TYPE>-----
///
/// ...
/// -----END PGP <TYPE>-----
/// ```
pub fn unarmor(armored: impl AsRef<[u8]>) -> crate::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(armored.as_ref().len());
    decode_to_buffer(armored.as_ref(), None, &mut output)?;
    Ok(output)
}

pub(crate) fn encode_to_buffer(
    input: &BinaryArmorSource<'_>,
    block_type: BlockType,
    include_checksum: bool,
    mut output: impl io::Write,
) -> Result<(), ArmorError> {
    armor::write(&input, block_type, &mut output, None, include_checksum)
        .map_err(ArmorError::Encode)
}

/// Unarmor the input into the output buffer.
///
/// If the `expected_type` is set, it checks if the armor header matches.
pub(crate) fn decode_to_buffer(
    input: &[u8],
    expected_type: Option<BlockType>,
    output: &mut Vec<u8>,
) -> Result<(), ArmorError> {
    let mut dearmor = decode_to_reader(input, expected_type)?;
    dearmor.read_to_end(output).map_err(ArmorError::Decode)?;
    Ok(())
}

/// Unarmor the input by reading from the reader.
pub(crate) fn decode_to_reader<R>(
    input: R,
    expected_type: Option<BlockType>,
) -> Result<Dearmor<R>, ArmorError>
where
    R: BufRead,
{
    let mut dearmor = Dearmor::new(input);
    dearmor
        .read_header()
        .map_err(|_| ArmorError::DecodeHeader)?;
    if let Some(expected_type) = expected_type {
        let typ = dearmor.typ.ok_or(ArmorError::DecodeHeader)?;
        if typ != expected_type {
            return Err(ArmorError::DecodeWrongHeader(
                typ.to_string(),
                expected_type,
            ));
        }
    }
    Ok(dearmor)
}

/// Internal function to decode a [`pgp::composed::Message`] from the input buffer.
pub(crate) fn decode_to_message(
    input: &[u8],
    data_encoding: ResolvedDataEncoding,
) -> Result<Message<'_>, MessageProcessingError> {
    match data_encoding {
        ResolvedDataEncoding::Armored => Message::from_armor(input)
            .map_err(MessageProcessingError::MessageParsing)
            .map(|value| value.0),
        ResolvedDataEncoding::Unarmored => {
            Message::from_bytes(input).map_err(MessageProcessingError::MessageParsing)
        }
    }
}

pub(crate) fn decode_to_message_reader<'a>(
    input: impl BufRead + std::fmt::Debug + 'a + Send,
    data_encoding: ResolvedDataEncoding,
) -> Result<Message<'a>, MessageProcessingError> {
    match data_encoding {
        ResolvedDataEncoding::Armored => Message::from_armor(input)
            .map_err(MessageProcessingError::MessageParsing)
            .map(|value| value.0),
        ResolvedDataEncoding::Unarmored => {
            Message::from_bytes(input).map_err(MessageProcessingError::MessageParsing)
        }
    }
}

/// Tries to heuristically detect if the input is armored.
pub(crate) fn detect_encoding(input: impl AsRef<[u8]>) -> ResolvedDataEncoding {
    const CHECK_ARMOR_PREFIX: &str = "-----BEGIN PGP ";
    let buffer = input.as_ref();

    if buffer.len() < CHECK_ARMOR_PREFIX.len() {
        return ResolvedDataEncoding::Unarmored;
    }

    if std::str::from_utf8(buffer).is_ok_and(|s| s.trim_start().starts_with(CHECK_ARMOR_PREFIX)) {
        ResolvedDataEncoding::Armored
    } else {
        ResolvedDataEncoding::Unarmored
    }
}

pub(crate) fn detect_encoding_reader(
    input: &mut impl BufRead,
) -> Result<ResolvedDataEncoding, io::Error> {
    Ok(detect_encoding(input.fill_buf()?))
}

pub(crate) struct BinaryArmorSource<'a> {
    data: &'a [u8],
}

impl Serialize for BinaryArmorSource<'_> {
    fn to_writer<W: io::Write>(&self, writer: &mut W) -> pgp::errors::Result<()> {
        writer.write_all(self.data)?;
        Ok(())
    }

    fn write_len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_encoding_armored() {
        let armored_data = include_bytes!("../test-data/keys/public_key_v4.asc");
        let encoding = detect_encoding(armored_data);
        assert_eq!(encoding, ResolvedDataEncoding::Armored);
    }

    #[test]
    fn test_detect_encoding_armored_with_leading_whitespace() {
        let armored_data = include_bytes!("../test-data/keys/public_key_v4.asc");
        let mut data_with_ws = b"\n  \r\n\t".to_vec();
        data_with_ws.extend_from_slice(armored_data);
        let encoding = detect_encoding(&data_with_ws);
        assert_eq!(encoding, ResolvedDataEncoding::Armored);
    }

    #[test]
    fn test_detect_encoding_unarmored() {
        let unarmored_data = include_bytes!("../test-data/messages/encrypted_message_v4_mail.bin");
        let encoding = detect_encoding(unarmored_data);
        assert_eq!(encoding, ResolvedDataEncoding::Unarmored);
    }

    #[test]
    fn test_detect_encoding_small_buffer() {
        let small_data = b"-----BE";
        let encoding = detect_encoding(small_data);
        assert_eq!(encoding, ResolvedDataEncoding::Unarmored);
    }
}

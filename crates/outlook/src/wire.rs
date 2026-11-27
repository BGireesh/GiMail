//! Serde adapters for Graph's encodings that serde can't express directly: `Edm.Binary` values
//! (an attachment's `contentBytes`) travel as base64.

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::{Deserialize, Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs};

/// What Graph sends and takes: the standard alphabet, padded.
const ENCODE: GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Decoders for either alphabet, padding optional, since OData's JSON format allows base64url.
const DECODE_STANDARD: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const DECODE_URL_SAFE: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// `bytes` as Graph's base64.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    ENCODE.encode(bytes)
}

/// Base64 in either alphabet, with or without padding.
///
/// # Errors
/// The decoder's error when `text` is base64 in neither alphabet.
pub fn decode_base64(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    DECODE_STANDARD
        .decode(text)
        .or_else(|_| DECODE_URL_SAFE.decode(text))
}

/// `serde_as` adapter for `Edm.Binary` (`Vec<u8>` ⇄ base64 string).
pub struct Base64;

impl SerializeAs<Vec<u8>> for Base64 {
    fn serialize_as<S: Serializer>(source: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_base64(source))
    }
}

impl<'de> DeserializeAs<'de, Vec<u8>> for Base64 {
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        decode_base64(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_standard_and_decodes_either() {
        assert_eq!(encode_base64(b"\xfb\xff"), "+/8=");
        for input in ["+/8=", "+/8", "-_8=", "-_8"] {
            assert_eq!(decode_base64(input).unwrap(), b"\xfb\xff", "{input}");
        }
    }
}

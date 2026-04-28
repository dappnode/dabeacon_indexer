//! SSZ bit-sequence decoders shared by the scanner pipeline.
//!
//! Attestations use a **bitlist** (variable length, sentinel-bit terminator); the
//! `committee_bits` and `sync_committee_bits` fields are **bitvectors** (fixed
//! length, no sentinel). Both decoders accept lower-case or upper-case hex with
//! an optional `0x` prefix and reject any malformed input as
//! [`Error::InconsistentBeaconData`] rather than falling back to empty output,
//! which would silently undercount participation.

use crate::error::{Error, Result};

/// Decode an SSZ bitlist (with sentinel bit) from hex. Used for `aggregation_bits`.
///
/// Rejects malformed hex, zero-length input, and a zero last byte — all spec
/// violations. The decoded vector's length is the number of *data* bits (i.e.
/// excludes the sentinel).
pub(super) fn decode_bitlist(hex_str: &str) -> Result<Vec<bool>> {
    let trimmed = hex_str.trim_start_matches("0x");
    let bytes = hex::decode(trimmed).map_err(|e| {
        Error::InconsistentBeaconData(format!("bitlist: invalid hex ({e}): {hex_str}"))
    })?;
    if bytes.is_empty() {
        return Err(Error::InconsistentBeaconData(
            "bitlist: empty input (no sentinel)".to_string(),
        ));
    }
    let last_byte = bytes[bytes.len() - 1];
    if last_byte == 0 {
        return Err(Error::InconsistentBeaconData(
            "bitlist: last byte is 0 — SSZ bitlist has no sentinel bit".to_string(),
        ));
    }

    let sentinel_pos = 7 - last_byte.leading_zeros() as usize;
    let mut bits = Vec::with_capacity((bytes.len() - 1) * 8 + sentinel_pos);
    for &byte in &bytes[..bytes.len() - 1] {
        for bit_idx in 0..8 {
            bits.push((byte >> bit_idx) & 1 == 1);
        }
    }
    for bit_idx in 0..sentinel_pos {
        bits.push((last_byte >> bit_idx) & 1 == 1);
    }
    Ok(bits)
}

/// Decode an SSZ bitvector (fixed size, no sentinel) from hex. Used for
/// `committee_bits` and `sync_committee_bits`. Caller is responsible for
/// verifying the resulting length matches the expected fixed size.
pub(super) fn decode_bitvector(hex_str: &str) -> Result<Vec<bool>> {
    let trimmed = hex_str.trim_start_matches("0x");
    let bytes = hex::decode(trimmed).map_err(|e| {
        Error::InconsistentBeaconData(format!("bitvector: invalid hex ({e}): {hex_str}"))
    })?;
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for &byte in &bytes {
        for bit_idx in 0..8 {
            bits.push((byte >> bit_idx) & 1 == 1);
        }
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitlist_empty_byte_errors() {
        assert!(matches!(
            decode_bitlist("0x00"),
            Err(Error::InconsistentBeaconData(_))
        ));
    }

    #[test]
    fn bitlist_no_bytes_errors() {
        assert!(matches!(
            decode_bitlist("0x"),
            Err(Error::InconsistentBeaconData(_))
        ));
    }

    #[test]
    fn bitlist_sentinel_only_yields_empty() {
        // 0x01 = sentinel at bit 0, no data bits — valid length-0 bitlist.
        assert!(decode_bitlist("0x01").unwrap().is_empty());
    }

    #[test]
    fn bitlist_single_data_bit() {
        assert_eq!(decode_bitlist("0x02").unwrap(), vec![false]);
        assert_eq!(decode_bitlist("0x03").unwrap(), vec![true]);
    }

    #[test]
    fn bitlist_three_bits_mixed() {
        // 0x0D = 0b00001101: sentinel at bit 3, data bits [1, 0, 1].
        assert_eq!(decode_bitlist("0x0D").unwrap(), vec![true, false, true]);
    }

    #[test]
    fn bitlist_multi_byte_full_first_byte() {
        assert_eq!(decode_bitlist("0xff01").unwrap(), vec![true; 8]);
    }

    #[test]
    fn bitlist_multi_byte_nine_bits() {
        let r = decode_bitlist("0xff03").unwrap();
        assert_eq!(r.len(), 9);
        assert!(r.iter().all(|&b| b));
    }

    #[test]
    fn bitlist_tolerates_missing_prefix() {
        assert_eq!(decode_bitlist("0D").unwrap(), vec![true, false, true]);
    }

    #[test]
    fn bitlist_malformed_hex_errors() {
        assert!(matches!(
            decode_bitlist("0xzz"),
            Err(Error::InconsistentBeaconData(_))
        ));
        assert!(matches!(
            decode_bitlist("not hex"),
            Err(Error::InconsistentBeaconData(_))
        ));
    }

    #[test]
    fn bitvector_single_byte_lsb_first() {
        assert_eq!(
            decode_bitvector("0x01").unwrap(),
            vec![true, false, false, false, false, false, false, false]
        );
        assert_eq!(decode_bitvector("0xff").unwrap(), vec![true; 8]);
    }

    #[test]
    fn bitvector_multi_byte_preserves_byte_order() {
        let r = decode_bitvector("0x0102").unwrap();
        assert_eq!(r.len(), 16);
        assert!(r[0]);
        assert!(r[9]);
        for (i, &bit) in r.iter().enumerate() {
            if i != 0 && i != 9 {
                assert!(!bit, "unexpected set bit at {i}");
            }
        }
    }

    #[test]
    fn bitvector_malformed_hex_errors() {
        assert!(matches!(
            decode_bitvector("0xzz"),
            Err(Error::InconsistentBeaconData(_))
        ));
    }
}

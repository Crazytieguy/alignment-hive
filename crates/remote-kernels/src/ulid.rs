//! Minimal ULID generation/validation used for machine identities.
//!
//! This module is intentionally tiny while the build environment cannot fetch
//! the upstream `ulid` crate. It emits the standard 48-bit millisecond time +
//! 80-bit randomness representation in canonical Crockford base32.

use rand::RngCore;

const ENCODING: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub fn new() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let timestamp = millis.min(u128::from((1_u64 << 48) - 1));
    let mut random = [0_u8; 10];
    rand::thread_rng().fill_bytes(&mut random);

    let mut random_value = 0_u128;
    for byte in random {
        random_value = (random_value << 8) | u128::from(byte);
    }
    let mut value = (timestamp << 80) | random_value;

    let mut encoded = [b'0'; 26];
    for slot in encoded.iter_mut().rev() {
        *slot = ENCODING[(value & 31) as usize];
        value >>= 5;
    }
    encoded.into_iter().map(char::from).collect()
}

pub fn is_valid(value: &str) -> bool {
    value.len() == 26
        && value.as_bytes()[0] <= b'7'
        && value.bytes().all(|byte| ENCODING.contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_canonical_and_distinct() {
        let first = new();
        let second = new();
        assert!(is_valid(&first), "{first}");
        assert!(is_valid(&second), "{second}");
        assert_ne!(first, second);
    }

    #[test]
    fn validation_rejects_legacy_and_ambiguous_forms() {
        assert!(!is_valid("main"));
        assert!(!is_valid("01ARZ3NDEKTSV4RRFFQ69G5FAI"));
        assert!(is_valid("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    }
}

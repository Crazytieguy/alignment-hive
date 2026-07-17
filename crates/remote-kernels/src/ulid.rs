//! Machine-identity ULIDs, backed by the `ulid` crate.
//!
//! `is_valid` is stricter than the crate's parser: record directory names
//! must round-trip byte-identically (canonical uppercase Crockford), so a
//! parseable-but-non-canonical form (e.g. lowercase) is rejected.

pub fn new() -> String {
    ulid::Ulid::new().to_string()
}

pub fn is_valid(value: &str) -> bool {
    ulid::Ulid::from_string(value).is_ok_and(|parsed| parsed.to_string() == value)
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
        assert_eq!(first.len(), 26);
    }

    #[test]
    fn validation_rejects_legacy_and_ambiguous_forms() {
        assert!(!is_valid("main"));
        // 'I' is outside Crockford base32.
        assert!(!is_valid("01ARZ3NDEKTSV4RRFFQ69G5FAI"));
        // Parseable but non-canonical (lowercase) must not validate.
        assert!(!is_valid("01arz3ndektsv4rrffq69g5fav"));
        assert!(is_valid("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    }
}

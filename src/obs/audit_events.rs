//! Audit event helpers shared across surfaces (G8 / bd-17c65.7.7).
//!
//! The action-string catalog itself lives in [`crate::db::audit_actions`]
//! alongside the existing write-surface constants so all audit producers
//! reference one source of truth. This module adds the
//! [`query_hash`] helper that every read surface must use when storing
//! a query reference in the audit `details` JSON — it produces a stable
//! BLAKE3 prefix so raw query text never crosses into the audit log.

/// Compute a deterministic short hash for the query text portion of a
/// search/context/pack audit row. Uses BLAKE3 over the UTF-8 bytes and
/// returns the lowercase hex prefix (16 chars = 64 bits, more than
/// enough to differentiate queries in audit-log analytics without
/// storing the raw text).
///
/// Privacy: this is the only sanctioned path for a query string to
/// reach the audit log. Callsites must never store the raw query.
#[must_use]
pub fn query_hash(query: &str) -> String {
    let digest = blake3::hash(query.as_bytes()).to_hex();
    format!("blake3:{}", &digest.as_str()[..16])
}

#[cfg(test)]
mod tests {
    use super::query_hash;

    #[test]
    fn query_hash_is_deterministic_and_prefixed() {
        let a = query_hash("forbidden dependencies");
        let b = query_hash("forbidden dependencies");
        assert_eq!(a, b);
        assert!(a.starts_with("blake3:"));
        assert_eq!(a.len(), "blake3:".len() + 16);
    }

    #[test]
    fn query_hash_differs_across_queries() {
        let a = query_hash("forbidden dependencies");
        let b = query_hash("prepare release");
        assert_ne!(a, b);
    }

    #[test]
    fn query_hash_is_empty_safe() {
        let h = query_hash("");
        assert!(h.starts_with("blake3:"));
    }

    #[test]
    fn query_hash_handles_unicode() {
        let h = query_hash("café résumé 中文 🚀");
        assert!(h.starts_with("blake3:"));
        assert_eq!(h.len(), "blake3:".len() + 16);
    }
}

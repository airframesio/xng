//! Channel-local aircraft-ID → ICAO cache.
//!
//! HFDL aircraft IDs (the 1-byte `ac_id` in MPDU/LPDU headers) are
//! assigned per-frequency by the ground station and are only meaningful
//! within that channel. dumphfdl resolves them to the real ICAO hex
//! address by remembering the ICAO carried in each logon-confirm LPDU
//! (`ac_cache.c`, keyed by `(freq, ac_id)`). Because each
//! [`crate::pdu::PduParser`] decodes a single channel, this cache keys on
//! `ac_id` alone — equivalent to dumphfdl's per-frequency keying scoped to
//! one channel.
//!
//! Entries expire after a TTL (dumphfdl `AC_CACHE_TTL_DEFAULT = 3600s`)
//! and are removed eagerly on logoff / logon-denied, since an aircraft can
//! only be logged on to one frequency at a time.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// dumphfdl `AC_CACHE_TTL_DEFAULT` (seconds).
pub const DEFAULT_TTL_SECS: u64 = 3600;

#[derive(Clone)]
struct Entry {
    icao: String,
    inserted: Instant,
}

/// Maps channel-local aircraft IDs to ICAO hex addresses with TTL expiry.
pub struct AcCache {
    map: HashMap<u8, Entry>,
    ttl: Duration,
}

impl AcCache {
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(DEFAULT_TTL_SECS))
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self { map: HashMap::new(), ttl }
    }

    /// Record a logon-confirm: `ac_id` on this channel is `icao`. Any
    /// stale entry for the same ICAO under a different ac_id is dropped so
    /// a re-logon under a new ID doesn't leave a phantom mapping.
    pub fn insert(&mut self, ac_id: u8, icao: &str) {
        self.remove_by_icao(icao);
        self.map.insert(ac_id, Entry { icao: icao.to_string(), inserted: Instant::now() });
    }

    /// Resolve an aircraft ID to its ICAO, honouring the TTL.
    pub fn lookup(&self, ac_id: u8) -> Option<&str> {
        self.map.get(&ac_id).and_then(|e| {
            if e.inserted.elapsed() <= self.ttl {
                Some(e.icao.as_str())
            } else {
                None
            }
        })
    }

    /// Drop the entry (if any) mapping to `icao` — used on logoff/denied,
    /// which are addressed by ICAO, not by the channel-local ac_id.
    pub fn remove_by_icao(&mut self, icao: &str) {
        self.map.retain(|_, e| e.icao != icao);
    }

    /// Number of live (non-expired) entries — for the cache gauge.
    pub fn len(&self) -> usize {
        self.map.values().filter(|e| e.inserted.elapsed() <= self.ttl).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for AcCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_lookup() {
        let mut c = AcCache::new();
        c.insert(0xC7, "040087");
        assert_eq!(c.lookup(0xC7), Some("040087"));
        assert_eq!(c.lookup(0x01), None);
    }

    #[test]
    fn remove_by_icao_clears_entry() {
        let mut c = AcCache::new();
        c.insert(0xC7, "040087");
        c.remove_by_icao("040087");
        assert_eq!(c.lookup(0xC7), None);
        assert!(c.is_empty());
    }

    #[test]
    fn relogon_under_new_id_drops_old_mapping() {
        let mut c = AcCache::new();
        c.insert(0x10, "04C11B");
        c.insert(0x20, "04C11B"); // same aircraft, new channel-local id
        assert_eq!(c.lookup(0x20), Some("04C11B"));
        assert_eq!(c.lookup(0x10), None, "stale id must be evicted");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn entries_expire_after_ttl() {
        let mut c = AcCache::with_ttl(Duration::from_millis(0));
        c.insert(0xC7, "040087");
        // A zero TTL means anything inserted in the past is already stale.
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(c.lookup(0xC7), None);
        assert!(c.is_empty());
    }
}

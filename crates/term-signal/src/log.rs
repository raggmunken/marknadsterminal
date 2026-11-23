//! The signal log. Every signal is written here before its outcome is
//! known, and each record carries a hash of the record before it. Editing
//! or removing an old record breaks every hash after it.
//!
//! This makes changes visible. It does not make them impossible: someone
//! with write access can rebuild the whole chain. Keeping a copy of the
//! latest hash somewhere else closes that gap.

use crate::Signal;
use sha2::{Digest, Sha256};
use term_core::LogRecord;

/// `prev_hash` of the first record.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LogError {
    #[error("signal is not valid: {0}")]
    Invalid(#[from] crate::SignalError),
    #[error("could not serialise signal: {0}")]
    Serialise(String),
    #[error("record {seq}: {reason}")]
    Broken { seq: u64, reason: &'static str },
}

pub fn hash_record(prev_hash: &str, created_at: i64, payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\n");
    hasher.update(created_at.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(payload.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the record that follows `prev`. The signal is validated first.
pub fn next_record(prev: Option<&LogRecord>, created_at: i64, signal: &Signal) -> Result<LogRecord, LogError> {
    signal.validate()?;
    let payload = serde_json::to_string(signal).map_err(|e| LogError::Serialise(e.to_string()))?;
    let (seq, prev_hash) = match prev {
        Some(p) => (p.seq + 1, p.hash.clone()),
        None => (1, GENESIS.to_string()),
    };
    let hash = hash_record(&prev_hash, created_at, &payload);
    Ok(LogRecord { seq, created_at, payload, prev_hash, hash })
}

/// Check the whole chain from the first record.
pub fn verify(records: &[LogRecord]) -> Result<(), LogError> {
    let mut prev_hash = GENESIS.to_string();
    let mut prev_time = i64::MIN;
    for (i, record) in records.iter().enumerate() {
        let seq = record.seq;
        if seq != i as u64 + 1 {
            return Err(LogError::Broken { seq, reason: "sequence number is out of order" });
        }
        if record.prev_hash != prev_hash {
            return Err(LogError::Broken { seq, reason: "does not link to the record before" });
        }
        if record.hash != hash_record(&record.prev_hash, record.created_at, &record.payload) {
            return Err(LogError::Broken { seq, reason: "content does not match its hash" });
        }
        if record.created_at < prev_time {
            return Err(LogError::Broken { seq, reason: "is dated before the record before" });
        }
        prev_hash = record.hash.clone();
        prev_time = record.created_at;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::example_signal;

    fn chain(n: usize) -> Vec<LogRecord> {
        let mut records: Vec<LogRecord> = Vec::new();
        for i in 0..n {
            let mut signal = example_signal();
            signal.instrument = format!("TEST{i}");
            let next = next_record(records.last(), 1000 + i as i64, &signal).unwrap();
            records.push(next);
        }
        records
    }

    #[test]
    fn hash_is_stable_and_sensitive() {
        let h = hash_record(GENESIS, 1, "{}");
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_record(GENESIS, 1, "{}"));
        assert_ne!(h, hash_record(GENESIS, 2, "{}"));
        assert_ne!(h, hash_record(GENESIS, 1, "{ }"));
        assert_ne!(h, hash_record(&h, 1, "{}"));
    }

    #[test]
    fn known_sha256_vector() {
        // sha256("abc"), a standard test vector, to prove the hashing is real.
        let digest: String = Sha256::digest(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn intact_chain_verifies() {
        assert_eq!(verify(&[]), Ok(()));
        let records = chain(5);
        assert_eq!(records[0].prev_hash, GENESIS);
        assert_eq!(records[4].seq, 5);
        assert_eq!(verify(&records), Ok(()));
    }

    #[test]
    fn edited_payload_is_detected() {
        let mut records = chain(5);
        records[2].payload = records[2].payload.replace("\"buy\"", "\"sell\"");
        assert_eq!(
            verify(&records),
            Err(LogError::Broken { seq: 3, reason: "content does not match its hash" })
        );
    }

    #[test]
    fn rehashed_edit_breaks_the_next_link() {
        let mut records = chain(5);
        records[2].payload = records[2].payload.replace("\"buy\"", "\"sell\"");
        records[2].hash = hash_record(&records[2].prev_hash, records[2].created_at, &records[2].payload);
        assert_eq!(
            verify(&records),
            Err(LogError::Broken { seq: 4, reason: "does not link to the record before" })
        );
    }

    #[test]
    fn removed_record_is_detected() {
        let mut records = chain(5);
        records.remove(1);
        assert!(matches!(verify(&records), Err(LogError::Broken { seq: 3, .. })));
    }

    #[test]
    fn backdated_record_is_detected() {
        let mut records = chain(2);
        let signal = example_signal();
        let backdated = next_record(records.last(), 5, &signal).unwrap();
        records.push(backdated);
        assert_eq!(
            verify(&records),
            Err(LogError::Broken { seq: 3, reason: "is dated before the record before" })
        );
    }

    #[test]
    fn invalid_signal_is_never_logged() {
        let mut signal = example_signal();
        signal.trigger.clear();
        assert!(matches!(next_record(None, 1, &signal), Err(LogError::Invalid(_))));
    }
}

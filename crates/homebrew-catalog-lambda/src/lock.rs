//! A lock shared with `scripts/publish.sh`, so that the update function and manual publishing never write the catalog
//! and site at the same time.
//!
//! The lock is an object in the site bucket, created with a conditional write (`If-None-Match: *`) so that only one
//! holder can create it. It records who holds it and until when; a lock past its expiry (left by a crash) can be taken
//! over with a write conditional on its ETag. It's released by a delete conditional on the holder's own ETag, so a
//! holder never removes a lock someone else has since taken over.

use {
    anyhow::{Context, Result},
    aws_sdk_s3::{error::ProvideErrorMetadata, primitives::ByteStream},
    serde::{Deserialize, Serialize},
    std::time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The lock's key in the site bucket. `scripts/publish.sh` uses the same key and format.
pub const LOCK_KEY: &str = ".publish.lock";

/// What the lock object holds, as compact JSON: `{"holder":"...","expires":<unix seconds>}`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LockInfo {
    pub holder: String,
    pub expires: u64,
}

impl LockInfo {
    fn is_expired(&self, now: u64) -> bool {
        now >= self.expires
    }
}

/// A lock this function holds.
#[derive(Debug)]
pub struct Lock {
    etag: String,
}

/// The result of trying to take the lock.
#[derive(Debug)]
pub enum Acquire {
    Acquired(Lock),
    /// Someone else holds an unexpired lock.
    Held(LockInfo),
}

/// Tries to take the lock for `holder`, for `ttl`.
pub async fn acquire(s3: &aws_sdk_s3::Client, bucket: &str, holder: &str, ttl: Duration) -> Result<Acquire> {
    let info = LockInfo {
        holder: holder.to_string(),
        expires: unix_now() + ttl.as_secs(),
    };
    let body = serde_json::to_vec(&info)?;

    // Two attempts: the lock may be released between finding it held and reading it.
    for _ in 0..2 {
        let created = s3
            .put_object()
            .bucket(bucket)
            .key(LOCK_KEY)
            .if_none_match("*")
            .body(ByteStream::from(body.clone()))
            .send()
            .await;
        match created {
            Ok(output) => {
                return Ok(Acquire::Acquired(Lock {
                    etag: output.e_tag().unwrap_or_default().to_string(),
                }));
            }
            Err(e) if is_conflict(e.code()) => {}
            Err(e) => return Err(e).with_context(|| format!("creating the lock s3://{bucket}/{LOCK_KEY}")),
        }

        let existing = match s3.get_object().bucket(bucket).key(LOCK_KEY).send().await {
            Ok(existing) => existing,
            Err(e) if e.as_service_error().is_some_and(|e| e.is_no_such_key()) => continue,
            Err(e) => return Err(e).with_context(|| format!("reading the lock s3://{bucket}/{LOCK_KEY}")),
        };
        let etag = existing.e_tag().unwrap_or_default().to_string();
        let bytes = existing.body.collect().await.context("reading the lock")?.into_bytes();
        // An unreadable lock is treated as expired rather than blocking updates forever.
        let held = serde_json::from_slice::<LockInfo>(&bytes).ok().filter(|held| !held.is_expired(unix_now()));
        if let Some(held) = held {
            return Ok(Acquire::Held(held));
        }

        // Expired: take it over, unless someone else just did.
        let taken = s3
            .put_object()
            .bucket(bucket)
            .key(LOCK_KEY)
            .if_match(&etag)
            .body(ByteStream::from(body.clone()))
            .send()
            .await;
        match taken {
            Ok(output) => {
                return Ok(Acquire::Acquired(Lock {
                    etag: output.e_tag().unwrap_or_default().to_string(),
                }));
            }
            Err(e) if is_conflict(e.code()) => continue,
            Err(e) => return Err(e).with_context(|| format!("taking over the expired lock s3://{bucket}/{LOCK_KEY}")),
        }
    }
    anyhow::bail!("the lock s3://{bucket}/{LOCK_KEY} kept changing; another update is probably starting")
}

/// Releases the lock, if it's still this holder's.
pub async fn release(s3: &aws_sdk_s3::Client, bucket: &str, lock: &Lock) -> Result<()> {
    match s3.delete_object().bucket(bucket).key(LOCK_KEY).if_match(&lock.etag).send().await {
        Ok(_) => Ok(()),
        // Someone took the lock over (after it expired); it's theirs now.
        Err(e) if is_conflict(e.code()) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("releasing the lock s3://{bucket}/{LOCK_KEY}")),
    }
}

/// Whether an S3 error code means a conditional write lost to another writer.
fn is_conflict(code: Option<&str>) -> bool {
    matches!(code, Some("PreconditionFailed" | "ConditionalRequestConflict"))
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_format_matches_publish_script() {
        // scripts/publish.sh writes and parses exactly this compact form.
        let info = LockInfo {
            holder: "publish.sh".into(),
            expires: 1_795_392_245,
        };
        assert_eq!(serde_json::to_string(&info).unwrap(), r#"{"holder":"publish.sh","expires":1795392245}"#);
        assert_eq!(serde_json::from_str::<LockInfo>(r#"{"holder":"publish.sh","expires":1795392245}"#).unwrap(), info);
    }

    #[test]
    fn expiry() {
        let info = LockInfo {
            holder: "x".into(),
            expires: 100,
        };
        assert!(!info.is_expired(99));
        assert!(info.is_expired(100));
    }

    #[test]
    fn conflict_codes() {
        assert!(is_conflict(Some("PreconditionFailed")));
        assert!(is_conflict(Some("ConditionalRequestConflict")));
        assert!(!is_conflict(Some("AccessDenied")));
        assert!(!is_conflict(None));
    }
}

//! Background history retries must not delay the independent recent-gap pass.
use std::time::{Duration, Instant};

use crate::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    Unavailable,
    InvalidData,
    Other,
}

impl FailureKind {
    fn of(error: &Error) -> Self {
        match error {
            Error::BackfillEpoch { source, .. } => Self::of(source),
            Error::BeaconDataUnavailable(_) | Error::BeaconApi { status: 404, .. } => {
                Self::Unavailable
            }
            Error::InconsistentBeaconData(_) | Error::InvalidBlockId(_) | Error::Json(_) => {
                Self::InvalidData
            }
            Error::Http(error) if error.is_decode() => Self::InvalidData,
            _ => Self::Other,
        }
    }
}

#[derive(Default)]
pub struct HistoricalRetry {
    next_attempt: Option<Instant>,
    failures: u32,
    last_kind: Option<FailureKind>,
}

impl HistoricalRetry {
    pub fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|next| now >= next)
    }

    /// Log once per failure category, with subsequent attempts at debug level.
    /// A change from unavailable history to invalid data must remain visible.
    /// Completion is reported by the backfill pass itself; no more retries are
    /// scheduled after it succeeds.
    pub fn failed(&mut self, error: &Error, now: Instant) {
        let delay = Duration::from_secs((60 * (1 << self.failures.min(5))).min(1800));
        self.next_attempt = Some(now + delay);
        self.failures = self.failures.saturating_add(1);
        let kind = FailureKind::of(error);
        let retry_in_seconds = delay.as_secs();
        if self.last_kind == Some(kind) {
            tracing::debug!(%error, retry_in_seconds, "Historical backfill still paused");
        } else {
            match kind {
                FailureKind::Unavailable => tracing::info!(
                    %error, retry_in_seconds,
                    "Historical backfill paused: required history unavailable; live collection continues. Retrying with backoff up to 30 minutes; an archive node may be needed"
                ),
                FailureKind::InvalidData => tracing::error!(
                    %error, retry_in_seconds,
                    "Historical backfill paused: invalid beacon data; live collection continues. Retrying with backoff up to 30 minutes"
                ),
                FailureKind::Other => tracing::warn!(
                    %error, retry_in_seconds,
                    "Historical backfill paused; live collection continues. Retrying with backoff up to 30 minutes"
                ),
            }
        }
        self.last_kind = Some(kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_are_delayed_and_capped() {
        let mut retry = HistoricalRetry::default();
        let mut now = Instant::now();
        let error = Error::BeaconDataUnavailable("pruned".into());
        assert!(retry.ready(now));
        for seconds in [60, 120, 240, 480, 960, 1800, 1800] {
            retry.failed(&error, now);
            let next = now + Duration::from_secs(seconds);
            assert!(!retry.ready(now));
            assert!(!retry.ready(next - Duration::from_millis(1)));
            assert!(retry.ready(next));
            now = next;
        }
    }

    #[test]
    fn missing_history_is_distinct_from_corruption_and_operational_errors() {
        for error in [
            Error::BeaconDataUnavailable("anchor missing".into()),
            Error::BeaconApi {
                status: 404,
                message: "state missing".into(),
            },
        ] {
            let contextual = Error::BackfillEpoch {
                epoch: 72063,
                source: Box::new(error),
            };
            assert_eq!(FailureKind::of(&contextual), FailureKind::Unavailable);
            assert!(contextual.to_string().contains("72063"));
        }
        assert_eq!(
            FailureKind::of(&Error::InconsistentBeaconData("root mismatch".into())),
            FailureKind::InvalidData
        );
        assert_eq!(
            FailureKind::of(&Error::BeaconApi {
                status: 503,
                message: "unavailable".into()
            }),
            FailureKind::Other
        );
    }

    #[test]
    fn repeated_failures_are_quiet_but_changed_failure_kind_is_reported() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut retry = HistoricalRetry::default();
            for error in [
                Error::BeaconDataUnavailable("anchor missing".into()),
                Error::BeaconDataUnavailable("state missing".into()),
                Error::InconsistentBeaconData("root mismatch".into()),
                Error::InconsistentBeaconData("root mismatch".into()),
            ] {
                retry.failed(&error, Instant::now());
            }
        });
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(log.lines().count(), 2, "{log}");
        assert_eq!(log.matches("INFO").count(), 1, "{log}");
        assert_eq!(log.matches("ERROR").count(), 1, "{log}");
        assert!(log.contains("root mismatch"), "{log}");
    }
}

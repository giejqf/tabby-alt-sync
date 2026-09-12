use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use time::error::ComponentRange;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

/// DRF's ISO-8601 rendering of an aware datetime normalised to UTC, with the
/// `+00:00` suffix replaced by `Z` (see `rest_framework/fields.py`,
/// `DateTimeField.to_representation`). Six fractional digits, always.
pub const DATETIME_FORMAT: &[FormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

/// `YYYY-MM-DD`, used for the `Unnamed config (…)` fallback name. tabby-web
/// computes it from `date.today()` with `TIME_ZONE = "UTC"`
/// (see `backend/tabby/app/models.py`, `backend/tabby/settings.py`).
pub const DATE_FORMAT: &[FormatItem<'_>] = format_description!("[year]-[month]-[day]");

#[derive(Debug, Error)]
pub enum TimeError {
    #[error("system clock is outside the representable range")]
    Clock,

    #[error("failed to format timestamp: {0}")]
    Format(#[from] time::error::Format),

    #[error("timestamp component out of range: {0}")]
    Range(#[from] ComponentRange),
}

fn now_utc() -> Result<OffsetDateTime, TimeError> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TimeError::Clock)?;
    Ok(OffsetDateTime::from_unix_timestamp_nanos(
        since_epoch.as_nanos() as i128,
    )?)
}

/// Current time in the exact wire format used for `created_at`/`modified_at`.
/// This is the only place timestamps are produced.
pub fn now_wire() -> Result<String, TimeError> {
    Ok(now_utc()?.format(DATETIME_FORMAT)?)
}

/// Current UTC date as `YYYY-MM-DD`.
pub fn today_wire() -> Result<String, TimeError> {
    Ok(now_utc()?.format(DATE_FORMAT)?)
}

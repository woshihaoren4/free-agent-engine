use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use fae_agent::TimerSchedule;
use std::str::FromStr;

pub(crate) fn parse(schedule: &TimerSchedule) -> anyhow::Result<(Schedule, Tz)> {
    let fields: Vec<_> = schedule.cron().split_whitespace().collect();
    let text = match fields.len() {
        5 => format!("0 {}", fields.join(" ")),
        6 | 7 => fields.join(" "),
        _ => anyhow::bail!("invalid_cron: expected 5, 6 or 7 fields"),
    };
    let cron = Schedule::from_str(&text).map_err(|e| anyhow::anyhow!("invalid_cron: {e}"))?;
    let tz = schedule
        .timezone()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid_timezone"))?;
    anyhow::ensure!(
        schedule.limit() != Some(0),
        "invalid_schedule: max_fires must be positive"
    );
    Ok((cron, tz))
}

// Iterate civil times in UTC, then resolve them explicitly in the selected zone.
// This avoids the cron iterator's ambiguous-time behavior at DST transitions.
pub(crate) fn next(
    schedule: &TimerSchedule,
    after: DateTime<Utc>,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let (cron, tz) = parse(schedule)?;
    let civil = after.with_timezone(&tz).naive_local().and_utc();
    let mut cursor = civil;
    loop {
        let Some(candidate) = cron.after(&cursor).next() else {
            return Ok(None);
        };
        cursor = candidate;
        let resolved = match tz.from_local_datetime(&candidate.naive_utc()) {
            LocalResult::None => continue,
            LocalResult::Single(t) => t.with_timezone(&Utc),
            LocalResult::Ambiguous(a, b) => a.min(b).with_timezone(&Utc),
        };
        if resolved > after {
            return Ok(Some(resolved));
        }
    }
}

pub(crate) fn latest(
    schedule: &TimerSchedule,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let (cron, tz) = parse(schedule)?;
    let civil = now.with_timezone(&tz).naive_local();
    // In the second fold, later civil times in the first fold have already
    // happened. Include that range when finding the most recent UTC occurrence.
    let fold = match tz.from_local_datetime(&civil) {
        LocalResult::Ambiguous(a, b) => b.signed_duration_since(a),
        _ => Duration::zero(),
    };
    let mut cursor = (civil + fold + Duration::seconds(1)).and_utc();
    loop {
        let Some(candidate) = cron.after(&cursor).next_back() else {
            return Ok(None);
        };
        cursor = candidate;
        let resolved = match tz.from_local_datetime(&candidate.naive_utc()) {
            LocalResult::None => continue,
            LocalResult::Single(t) => t.with_timezone(&Utc),
            LocalResult::Ambiguous(a, b) => a.min(b).with_timezone(&Utc),
        };
        if resolved <= now {
            return Ok(Some(resolved));
        }
    }
}
pub(crate) fn timestamp(text: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc))
}

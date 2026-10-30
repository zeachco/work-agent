use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use cron::Schedule;
use std::str::FromStr;

/// Accepts 5-field (minute-first) or 6-field (second-first) expressions.
pub fn parse(expr: &str) -> Result<Schedule> {
    let fields = expr.split_whitespace().count();
    let normalized = match fields {
        5 => format!("0 {expr}"),
        6 | 7 => expr.to_string(),
        _ => anyhow::bail!("expected 5 or 6 cron fields, got {fields}: `{expr}`"),
    };
    Schedule::from_str(&normalized).with_context(|| format!("invalid cron `{expr}`"))
}

/// A task is due when at least one scheduled tick has passed since it last ran.
/// A task that never ran is due immediately.
pub fn is_due(expr: &str, last_run: Option<DateTime<Local>>, now: DateTime<Local>) -> Result<bool> {
    Ok(match last_run {
        None => true,
        Some(last) => next_after(expr, last)?.map(|next| next <= now).unwrap_or(false),
    })
}

/// The first scheduled tick strictly after `from`, or `None` when the
/// expression has no future occurrence.
pub fn next_after(expr: &str, from: DateTime<Local>) -> Result<Option<DateTime<Local>>> {
    Ok(parse(expr)?.after(&from).next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn five_field_is_accepted() {
        assert!(parse("*/5 * * * *").is_ok());
        assert!(parse("0 9 * * 1-5").is_ok());
        assert!(parse("bogus").is_err());
    }

    #[test]
    fn due_after_interval() {
        let last = Local.with_ymd_and_hms(2026, 10, 21, 9, 0, 0).unwrap();
        assert!(!is_due("0 * * * *", Some(last), last + chrono::Duration::minutes(30)).unwrap());
        assert!(is_due("0 * * * *", Some(last), last + chrono::Duration::minutes(61)).unwrap());
        assert!(is_due("0 * * * *", None, last).unwrap());
    }

    #[test]
    fn next_after_is_strictly_later() {
        let at = Local.with_ymd_and_hms(2026, 10, 21, 9, 0, 0).unwrap();
        let next = next_after("0 * * * *", at).unwrap().unwrap();
        assert_eq!(next, at + chrono::Duration::hours(1));
    }
}

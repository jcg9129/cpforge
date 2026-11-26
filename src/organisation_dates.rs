//! Local-day filtering. Stored dates stay dates; only instants use zone rules.
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use toml_edit::Item;

use crate::cli::Filters;

fn ranges(filters: &Filters) -> [(&str, Option<&str>); 4] {
    [
        ("created_at", filters.created.as_deref()),
        ("updated_at", filters.updated.as_deref()),
        ("solved_at", filters.solved.as_deref()),
        ("archived_at", filters.archived_in.as_deref()),
    ]
}

/// Resolve once, before evaluating any candidate. No date filter means no
/// system lookup and no warning, even when the environment's TZ is invalid.
pub(crate) fn zone(filters: &Filters) -> TimeZone {
    if !filters.has_dates() {
        return TimeZone::UTC;
    }
    match discover() {
        Ok(zone) => zone,
        Err(cause) => {
            eprintln!("cpf: warning: time zone lookup failed: {cause}; using UTC for date filters");
            TimeZone::UTC
        }
    }
}

fn discover() -> Result<TimeZone, String> {
    // Jiff handles TZ names, paths and POSIX rules on every OS, and system
    // discovery on Unix. main normalizes empty TZ to unset before workers start.
    #[cfg(windows)]
    if std::env::var_os("TZ").is_none_or(|value| value.is_empty()) {
        use windows_sys::Win32::System::Time::{
            DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, TIME_ZONE_ID_INVALID,
        };
        let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
        // SAFETY: info is valid writable storage for the entire system call.
        if unsafe { GetDynamicTimeZoneInformation(&mut info) } == TIME_ZONE_ID_INVALID {
            return Err(format!(
                "GetDynamicTimeZoneInformation: {}",
                std::io::Error::last_os_error()
            ));
        }
        let rules = WindowsTimeZone {
            disabled: info.DynamicDaylightTimeDisabled,
            daylight_month: info.DaylightDate.wMonth,
            bias: info.Bias,
            _standard_bias: info.StandardBias,
        };
        if let Some(seconds) = windows_fixed_offset(rules) {
            let offset = jiff::tz::Offset::from_seconds(seconds).map_err(|e| e.to_string())?;
            return Ok(TimeZone::fixed(offset));
        }
    }
    TimeZone::try_system().map_err(|cause| {
        let source = match std::env::var_os("TZ") {
            Some(value) if !value.is_empty() => format!("TZ={value:?}"),
            _ if cfg!(windows) => "Windows system time zone".into(),
            _ => "/etc/localtime".into(),
        };
        format!("{source}: {cause}")
    })
}

/// Relevant fields of DYNAMIC_TIME_ZONE_INFORMATION, portable for pure tests.
#[cfg(any(windows, test))]
struct WindowsTimeZone {
    disabled: bool,
    daylight_month: u16,
    bias: i32,
    // Ignored when transition dates are absent, including when it is nonzero.
    _standard_bias: i32,
}

/// Only a disabled zone without transitions uses Bias alone.
#[cfg(any(windows, test))]
fn windows_fixed_offset(info: WindowsTimeZone) -> Option<i32> {
    (info.disabled && info.daylight_month == 0).then_some(info.bias.saturating_mul(-60))
}

fn local_date(text: &str, zone: &TimeZone) -> Option<Date> {
    if !crate::organisation::timestamp_valid(text) {
        return None;
    }
    if text.len() == 10 {
        text.parse().ok()
    } else {
        text.parse::<Timestamp>()
            .ok()
            .map(|instant| zone.to_datetime(instant).date())
    }
}

fn in_range(date: Date, range: &str) -> bool {
    let (start, end) = range.split_once("..").unwrap_or((range, range));
    // Clap has checked the grammar before any discovery or filesystem access.
    (start.is_empty() || date >= start.parse::<Date>().expect("validated range start"))
        && (end.is_empty() || date <= end.parse::<Date>().expect("validated range end"))
}

/// The evaluator's zone comes from discovery in production and repository
/// POSIX/TZif data in tests. Neither date-only values nor stored bytes change.
pub(crate) fn matches(filters: &Filters, fields: Option<&Item>, zone: &TimeZone) -> bool {
    ranges(filters).into_iter().all(|(field, range)| {
        range.is_none_or(|range| {
            fields
                .and_then(|fields| fields.get(field))
                .and_then(Item::as_str)
                .and_then(|text| local_date(text, zone))
                .is_some_and(|date| in_range(date, range))
        })
    })
}

#[cfg(test)]
#[path = "organisation_dates_tests.rs"]
mod tests;

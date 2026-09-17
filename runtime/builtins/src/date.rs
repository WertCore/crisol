//! `Date`, as a time value and the arithmetic on it.
//!
//! A `Date` is one number: milliseconds since 1970-01-01T00:00:00Z. Everything else —
//! `getMonth`, `setDate`, the constructor's seven arguments — is arithmetic on that number.
//!
//! # `Invalid Date` is a real, reachable state
//!
//! The time value must be an **integer** with magnitude at most `8.64e15` (100 million days
//! either side of the epoch). Anything else — a fraction, a larger number, `NaN` — becomes
//! `NaN`, and the `Date` is permanently invalid. `new Date(8.64e15 + 1).getTime()` is `NaN`,
//! not a clamped value.
//!
//! That is `TimeClip`, and it must run on **every** construction and mutation. An
//! implementation that clamped instead of invalidating would let a date silently become a
//! different date, which is worse than an obviously broken one.
//!
//! # Months wrap; days are 1-based and months are not
//!
//! `new Date(2020, 12, 1)` is **January 2021**, because month 12 is one past December (month
//! 11). `new Date(2020, 0, 0)` is **31 December 2019**, because day 0 is the day before the
//! first. Both are specified, both are used deliberately — `new Date(y, m + 1, 0)` is the
//! idiomatic "last day of month m".
//!
//! And `getMonth` is **0-based** while `getDate` is **1-based**. That inconsistency is in the
//! language, not in this implementation, and normalising it here would make every ported
//! program wrong by one month.
//!
//! # Only UTC
//!
//! Local-time accessors need the host's zone and its historical transition table, which is
//! M15's to supply. Implementing them here against a guess would produce a date that is right
//! in one timezone and silently wrong in the rest.

/// The largest magnitude a time value may have: 100 million days in milliseconds.
pub const MAX_TIME: f64 = 8.64e15;

/// Milliseconds in a day.
pub const MS_PER_DAY: f64 = 86_400_000.0;

/// `TimeClip` — the rule that makes `Invalid Date` reachable.
///
/// Returns `NaN` for a non-finite value, for one beyond [`MAX_TIME`], and truncates a
/// fractional one toward zero. Clamping instead would let a date silently become a *different*
/// date, which is worse than an obviously broken one.
#[must_use]
pub fn time_clip(time: f64) -> f64 {
    if !time.is_finite() || time.abs() > MAX_TIME {
        return f64::NAN;
    }
    // Truncate toward zero, not floor: -1.5 ms becomes -1, matching `ToIntegerOrInfinity`.
    let truncated = time.trunc();
    // `-0` is normalised to `0`, so two dates one millisecond either side of the epoch do not
    // compare unequal through a sign nobody can see.
    if truncated == 0.0 { 0.0 } else { truncated }
}

/// Whether a year is a leap year.
///
/// Divisible by 4, except centuries, unless divisible by 400. 1900 was not a leap year and
/// 2000 was — the case that a naive `% 4` gets wrong once a century and that shipped in a
/// great many spreadsheets.
#[must_use]
pub fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in a month, 0-based as JavaScript numbers them.
#[must_use]
pub fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        0 | 2 | 4 | 6 | 7 | 9 | 11 => 31,
        3 | 5 | 8 | 10 => 30,
        1 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days since the epoch for a civil date, with **month 0 = January**.
///
/// Months and days outside their usual range are *not* errors: they roll over, which is what
/// makes `new Date(y, m + 1, 0)` the last day of month `m`.
#[must_use]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // Normalise the month first, carrying into the year. This is what makes month 12 mean
    // January of the next year rather than a failure.
    let year = year + month.div_euclid(12);
    let month = month.rem_euclid(12);

    // Howard Hinnant's civil-from-days, shifted so March starts the year and the leap day lands
    // at the end. `day` is left un-normalised and added at the end, so day 0 and day 32 both
    // roll the way the specification says.
    let y = if month <= 1 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let month_shifted = if month <= 1 { month + 10 } else { month - 2 };
    let day_of_year = (153 * month_shifted + 2) / 5;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468 + (day - 1)
}

/// The civil date for a day count, as `(year, month, day)` with month 0-based.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shifted + 2) / 5 + 1;
    let month = if month_shifted < 10 {
        month_shifted + 2
    } else {
        month_shifted - 10
    };
    let year = if month <= 1 { year + 1 } else { year };
    (year, month, day)
}

/// `MakeTime` — milliseconds within a day. Components roll over rather than erroring.
#[must_use]
pub fn make_time(hour: f64, minute: f64, second: f64, millisecond: f64) -> f64 {
    hour.mul_add(3_600_000.0, minute * 60_000.0) + second.mul_add(1000.0, millisecond)
}

/// `MakeDate` — a day count and a time within it, combined.
#[must_use]
pub fn make_date(day: f64, time: f64) -> f64 {
    day.mul_add(MS_PER_DAY, time)
}

/// A full time value from civil components, with `Invalid Date` for anything out of range.
///
/// Month and day roll over: `(2020, 12, 1)` is January 2021 and `(2020, 0, 0)` is 31 December
/// 2019.
#[must_use]
pub fn time_from_civil(
    year: i64,
    month: i64,
    day: i64,
    hour: f64,
    minute: f64,
    second: f64,
    millisecond: f64,
) -> f64 {
    let days = days_from_civil(year, month, day);
    #[expect(
        clippy::cast_precision_loss,
        reason = "TimeClip rejects anything past 8.64e15 ms, far inside f64's exact-integer range"
    )]
    let combined = make_date(days as f64, make_time(hour, minute, second, millisecond));
    time_clip(combined)
}

/// The day number a time value falls in, flooring so that times before the epoch go the right
/// way.
///
/// Truncating instead would put 1969-12-31T23:00Z in day 0 rather than day -1, which is the
/// classic off-by-one for every date before 1970.
#[must_use]
pub fn day_from_time(time: f64) -> f64 {
    (time / MS_PER_DAY).floor()
}

/// Milliseconds within the day, always non-negative.
///
/// `rem_euclid` rather than `%`: for a negative time value the plain remainder is negative,
/// and an hour of -1 is not a time of day.
#[must_use]
pub fn time_within_day(time: f64) -> f64 {
    time.rem_euclid(MS_PER_DAY)
}

/// The day of the week, `0` = Sunday.
///
/// The epoch was a **Thursday**, which is why the offset is 4 and not 0. Getting that constant
/// wrong shifts every weekday in the program by a fixed amount, which looks like a timezone
/// bug and is not.
#[must_use]
pub fn week_day(time: f64) -> Option<u8> {
    if time.is_nan() {
        return None;
    }
    let day = day_from_time(time);
    let weekday = (day + 4.0).rem_euclid(7.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rem_euclid(7) is in 0..7"
    )]
    Some(weekday as u8)
}

/// The broken-down UTC fields of a time value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    /// Full year, not offset by 1900.
    pub year: i64,
    /// **0-based**, as `getMonth` reports it.
    pub month: i64,
    /// **1-based**, as `getDate` reports it.
    pub day: i64,
    /// Hour, 0-23.
    pub hour: i64,
    /// Minute, 0-59.
    pub minute: i64,
    /// Second, 0-59.
    pub second: i64,
    /// Millisecond, 0-999.
    pub millisecond: i64,
    /// 0 = Sunday.
    pub week_day: u8,
}

/// Breaks a time value into UTC fields, or `None` for an invalid date.
///
/// `None` rather than zeroes: every accessor on an `Invalid Date` reports `NaN`, and returning
/// plausible-looking components would let an invalid date print as 1 January 1970.
#[must_use]
pub fn fields(time: f64) -> Option<Fields> {
    if time.is_nan() {
        return None;
    }
    let day = day_from_time(time);
    let within = time_within_day(time);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "both are bounded by TimeClip's 8.64e15 ms limit"
    )]
    let (year, month, date) = civil_from_days(day as i64);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "rem_euclid keeps this under 86_400_000 and non-negative"
    )]
    let ms = within as i64;
    Some(Fields {
        year,
        month,
        day: date,
        hour: ms / 3_600_000,
        minute: (ms / 60_000) % 60,
        second: (ms / 1000) % 60,
        millisecond: ms % 1000,
        week_day: week_day(time)?,
    })
}

/// `Date.prototype.toISOString`, or `None` for an invalid date.
///
/// `toISOString` **throws** a `RangeError` on an invalid date rather than returning
/// `"Invalid Date"` — unlike `toString`, which returns it. Two methods on the same object with
/// different failure modes, and reporting `None` lets the caller pick the right one.
#[must_use]
pub fn to_iso_string(time: f64) -> Option<String> {
    let f = fields(time)?;
    // Years outside 0..9999 use the expanded six-digit form with a mandatory sign.
    let year = if (0..=9999).contains(&f.year) {
        format!("{:04}", f.year)
    } else {
        format!("{}{:06}", if f.year < 0 { '-' } else { '+' }, f.year.abs())
    };
    Some(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        f.month + 1,
        f.day,
        f.hour,
        f.minute,
        f.second,
        f.millisecond
    ))
}

/// `Date.prototype.toString` for an invalid date.
pub const INVALID_DATE: &str = "Invalid Date";

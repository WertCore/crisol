//! `Date` time arithmetic.
//!
//! The recurring failure here is **before 1970**: a truncating division puts
//! 1969-12-31T23:00Z in day 0 rather than day -1, and a plain remainder gives it a negative
//! hour. Both look right for every date anyone tests by hand.

use crisol_builtins::{
    INVALID_DATE, MAX_TIME, MS_PER_DAY, civil_from_days, day_from_time, days_from_civil,
    days_in_month, fields, is_leap_year, make_time, time_clip, time_from_civil, time_within_day,
    to_iso_string, week_day,
};

// ---- TimeClip, which makes Invalid Date reachable -------------------------------------------

#[test]
fn a_time_value_beyond_the_limit_becomes_invalid_not_clamped() {
    // Clamping would let a date silently become a *different* date, which is worse than an
    // obviously broken one.
    assert_eq!(time_clip(MAX_TIME), MAX_TIME, "the limit itself is fine");
    assert!(time_clip(MAX_TIME + 1.0).is_nan());
    assert!(time_clip(-MAX_TIME - 1.0).is_nan());
    assert!(time_clip(f64::INFINITY).is_nan());
    assert!(time_clip(f64::NAN).is_nan());
}

#[test]
fn a_fractional_time_value_truncates_toward_zero() {
    assert_eq!(time_clip(1.9), 1.0);
    assert_eq!(time_clip(-1.9), -1.0, "toward zero, not floor");
}

#[test]
fn negative_zero_is_normalised() {
    // So two dates either side of the epoch do not compare unequal through a sign nobody can
    // see.
    let clipped = time_clip(-0.0);
    assert_eq!(clipped, 0.0);
    assert!(clipped.is_sign_positive());
}

// ---- leap years -------------------------------------------------------------------------------

#[test]
fn the_century_rule_is_not_just_divisible_by_four() {
    assert!(is_leap_year(2020));
    assert!(!is_leap_year(2021));
    assert!(!is_leap_year(1900), "a century that is not a leap year");
    assert!(is_leap_year(2000), "a century that is");
    assert_eq!(days_in_month(2020, 1), 29, "February in a leap year");
    assert_eq!(days_in_month(1900, 1), 28);
}

// ---- wrapping, which is deliberate ------------------------------------------------------------

#[test]
fn month_twelve_is_january_of_the_next_year() {
    // `new Date(2020, 12, 1)` is January 2021. Month 12 is one past December (month 11).
    let wrapped = time_from_civil(2020, 12, 1, 0.0, 0.0, 0.0, 0.0);
    let january = time_from_civil(2021, 0, 1, 0.0, 0.0, 0.0, 0.0);
    assert_eq!(wrapped, january);
}

#[test]
fn a_negative_month_goes_backwards() {
    let back = time_from_civil(2021, -1, 1, 0.0, 0.0, 0.0, 0.0);
    let december = time_from_civil(2020, 11, 1, 0.0, 0.0, 0.0, 0.0);
    assert_eq!(back, december);
}

#[test]
fn day_zero_is_the_last_day_of_the_previous_month() {
    // Which is what makes `new Date(y, m + 1, 0)` the idiomatic "last day of month m".
    let last_of_february = fields(time_from_civil(2020, 2, 0, 0.0, 0.0, 0.0, 0.0)).expect("valid");
    assert_eq!(
        (
            last_of_february.year,
            last_of_february.month,
            last_of_february.day
        ),
        (2020, 1, 29),
        "29 February 2020, because 2020 is a leap year"
    );

    let last_of_january = fields(time_from_civil(2020, 1, 0, 0.0, 0.0, 0.0, 0.0)).expect("valid");
    assert_eq!(last_of_january.day, 31);
}

#[test]
fn a_day_past_the_end_rolls_into_the_next_month() {
    let rolled = fields(time_from_civil(2021, 1, 29, 0.0, 0.0, 0.0, 0.0)).expect("valid");
    assert_eq!(
        (rolled.month, rolled.day),
        (2, 1),
        "29 February 2021 does not exist, so it is 1 March"
    );
}

// ---- before the epoch, where this usually breaks -------------------------------------------------

#[test]
fn the_day_before_the_epoch_is_day_minus_one() {
    // A truncating division would put this in day 0 — the classic off-by-one for every date
    // before 1970.
    let hour_before = -3_600_000.0;
    assert_eq!(day_from_time(hour_before), -1.0);
    assert_eq!(day_from_time(0.0), 0.0);
    assert_eq!(day_from_time(-MS_PER_DAY), -1.0);
    assert_eq!(day_from_time(-MS_PER_DAY - 1.0), -2.0);
}

#[test]
fn the_time_of_day_is_never_negative() {
    // A plain remainder gives a negative hour for a pre-epoch time, and an hour of -1 is not a
    // time of day.
    let hour_before = -3_600_000.0;
    assert_eq!(time_within_day(hour_before), 23.0 * 3_600_000.0);
    assert!(time_within_day(-1.0) > 0.0);
}

#[test]
fn a_date_before_the_epoch_breaks_down_correctly() {
    let new_years_eve = fields(-3_600_000.0).expect("valid");
    assert_eq!(
        (
            new_years_eve.year,
            new_years_eve.month,
            new_years_eve.day,
            new_years_eve.hour
        ),
        (1969, 11, 31, 23)
    );
}

// ---- the weekday constant -----------------------------------------------------------------------

#[test]
fn the_epoch_was_a_thursday() {
    // Which is why the offset is 4 and not 0. Getting it wrong shifts every weekday in the
    // program by a fixed amount, which looks like a timezone bug and is not.
    assert_eq!(week_day(0.0), Some(4), "1970-01-01 was a Thursday");
    assert_eq!(week_day(MS_PER_DAY), Some(5), "Friday");
    assert_eq!(week_day(-MS_PER_DAY), Some(3), "Wednesday");
    assert_eq!(week_day(3.0 * MS_PER_DAY), Some(0), "Sunday");
}

// ---- round trips ---------------------------------------------------------------------------------

#[test]
fn civil_and_day_counts_round_trip() {
    for (year, month, day) in [
        (1970, 0, 1),
        (1969, 11, 31),
        (2000, 1, 29),
        (1900, 1, 28),
        (2024, 5, 15),
        (1, 0, 1),
        (-1, 0, 1),
    ] {
        let days = days_from_civil(year, month, day);
        assert_eq!(
            civil_from_days(days),
            (year, month, day),
            "{year}-{month}-{day}"
        );
    }
}

#[test]
fn the_epoch_is_day_zero() {
    assert_eq!(days_from_civil(1970, 0, 1), 0);
    assert_eq!(civil_from_days(0), (1970, 0, 1));
}

#[test]
fn make_time_combines_its_parts() {
    assert_eq!(make_time(1.0, 2.0, 3.0, 4.0), 3_723_004.0);
    assert_eq!(make_time(0.0, 0.0, 0.0, 0.0), 0.0);
}

// ---- rendering ---------------------------------------------------------------------------------

#[test]
fn iso_strings_are_fixed_width() {
    assert_eq!(
        to_iso_string(0.0).as_deref(),
        Some("1970-01-01T00:00:00.000Z")
    );
    assert_eq!(
        to_iso_string(time_from_civil(2024, 5, 15, 13.0, 45.0, 30.0, 123.0)).as_deref(),
        Some("2024-06-15T13:45:30.123Z"),
        "month is 0-based inside and 1-based in the text"
    );
}

#[test]
fn years_outside_four_digits_use_the_expanded_form() {
    let far = time_from_civil(12_345, 0, 1, 0.0, 0.0, 0.0, 0.0);
    assert!(
        to_iso_string(far).expect("valid").starts_with("+012345-"),
        "a mandatory sign and six digits"
    );
}

#[test]
fn an_invalid_date_has_no_fields_and_no_iso_string() {
    // Every accessor on an Invalid Date reports NaN; returning plausible components would let
    // it print as 1 January 1970.
    assert_eq!(fields(f64::NAN), None);
    assert_eq!(to_iso_string(f64::NAN), None);
    assert_eq!(week_day(f64::NAN), None);
    assert_eq!(INVALID_DATE, "Invalid Date");
}

#[test]
fn a_value_past_the_limit_produces_an_invalid_date_end_to_end() {
    let too_far = time_from_civil(300_000, 0, 1, 0.0, 0.0, 0.0, 0.0);
    assert!(too_far.is_nan(), "beyond 100 million days from the epoch");
    assert_eq!(fields(too_far), None);
}

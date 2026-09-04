//! The `core/time` schema.
//!
//! # What is authoritative, and what is not
//!
//! A time's meaning is carried entirely by its `value`, `format` and
//! `scale`, and those three round-trip losslessly. The calendar breakdown
//! this module also computes is a *convenience*, and for anything not on the
//! UTC scale it is approximate: converting exactly requires a leap-second
//! table, which this library does not carry, so the atomic-scale formats
//! (`gps`, `tai_seconds`, `unix_tai`, `cxcsec`) and the non-UTC scales come
//! out offset by the relevant amount. libasdf documents the same caveat.
//!
//! # The `format` / `base_format` split
//!
//! The schema's `format` field admits only a subset of astropy's formats.
//! The rest -- `isot`, `fits`, `datetime`, `plot_date`, `ymdhms`,
//! `datetime64`, `jyear_str`, `byear_str` -- may appear only in
//! `base_format`. Reading collapses the pair into one effective format;
//! writing splits it back out.

use asdf_yaml::{Document, NodeData, NodeId, Resolved};

use crate::error::{Result, err};

/// How a time is written, mirroring `asdf_time_format_t`.
///
/// The discriminants are part of the C ABI.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum TimeFormat {
    /// ISO 8601 date and time, the default.
    #[default]
    Iso = 0,
    /// Year, day-of-year and time.
    Yday,
    /// Besselian epoch year.
    Byear,
    /// Julian epoch year.
    Jyear,
    /// Decimal year.
    DecimalYear,
    /// Julian Date.
    Jd,
    /// Modified Julian Date.
    Mjd,
    /// Seconds from the GPS epoch.
    Gps,
    /// Seconds from the Unix epoch, ignoring leap seconds.
    Unix,
    /// UT seconds from 1979-01-01.
    Utime,
    /// SI seconds from 1958-01-01, including leap seconds.
    TaiSeconds,
    /// Chandra X-ray Center seconds from 1998-01-01 TT.
    Cxcsec,
    /// GALEX seconds from 1980-01-06.
    Galexsec,
    /// SI seconds from 1970-01-01 TAI.
    UnixTai,
    /// Reserved; not a usable format.
    Reserved1,
    /// Besselian epoch in string form.
    ByearStr,
    /// A Python `datetime.datetime`.
    Datetime,
    /// FITS date-time string.
    Fits,
    /// ISO 8601 with a literal `T` separator.
    Isot,
    /// Julian epoch in string form.
    JyearStr,
    /// matplotlib ordinal days.
    PlotDate,
    /// Year/month/day/hour/minute/second fields.
    Ymdhms,
    /// NumPy `datetime64`.
    Datetime64,
}

/// The time scale, mirroring `asdf_time_scale_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(i32)]
pub enum TimeScale {
    /// Coordinated Universal Time, the default.
    #[default]
    Utc = 0,
    /// International Atomic Time.
    Tai,
    /// Barycentric Coordinate Time.
    Tcb,
    /// Geocentric Coordinate Time.
    Tcg,
    /// Barycentric Dynamical Time.
    Tdb,
    /// Terrestrial Time.
    Tt,
    /// Universal Time.
    Ut1,
}

/// Format names as they appear in a file, indexed by discriminant.
///
/// `Reserved1` has no name, matching upstream's `NULL` entry.
const FORMAT_NAMES: [Option<&str>; 23] = [
    Some("iso"),
    Some("yday"),
    Some("byear"),
    Some("jyear"),
    Some("decimalyear"),
    Some("jd"),
    Some("mjd"),
    Some("gps"),
    Some("unix"),
    Some("utime"),
    Some("tai_seconds"),
    Some("cxcsec"),
    Some("galexsec"),
    Some("unix_tai"),
    None,
    Some("byear_str"),
    Some("datetime"),
    Some("fits"),
    Some("isot"),
    Some("jyear_str"),
    Some("plot_date"),
    Some("ymdhms"),
    Some("datetime64"),
];

const SCALE_NAMES: [&str; 7] = ["utc", "tai", "tcb", "tcg", "tdb", "tt", "ut1"];

impl TimeFormat {
    /// The name written to a file, or `None` for the reserved slot.
    pub fn name(self) -> Option<&'static str> {
        FORMAT_NAMES.get(self as usize).copied().flatten()
    }

    /// Parse a format name.
    pub fn from_name(name: &str) -> Option<Self> {
        FORMAT_NAMES
            .iter()
            .position(|candidate| *candidate == Some(name))
            .and_then(Self::from_index)
    }

    fn from_index(index: usize) -> Option<Self> {
        (index < FORMAT_NAMES.len()).then(|| {
            // Every index below the table's length is a valid discriminant.
            unsafe_transmute_format(index as i32)
        })
    }

    /// The format written to the wire `format` field.
    ///
    /// The schema only permits a subset there; an "other" format maps to the
    /// standard one it is a spelling of, and goes in `base_format` instead.
    pub fn standard(self) -> Self {
        match self {
            TimeFormat::Isot
            | TimeFormat::Fits
            | TimeFormat::Datetime
            | TimeFormat::PlotDate
            | TimeFormat::Ymdhms
            | TimeFormat::Datetime64 => TimeFormat::Iso,
            TimeFormat::JyearStr => TimeFormat::Jyear,
            TimeFormat::ByearStr => TimeFormat::Byear,
            other => other,
        }
    }

    /// Whether this format may appear only in `base_format`.
    pub fn is_other(self) -> bool {
        self.standard() != self
    }
}

/// Convert an index to a format without `unsafe`.
///
/// A plain match keeps the mapping explicit and checkable, which matters
/// because the discriminants are ABI.
fn unsafe_transmute_format(value: i32) -> TimeFormat {
    match value {
        0 => TimeFormat::Iso,
        1 => TimeFormat::Yday,
        2 => TimeFormat::Byear,
        3 => TimeFormat::Jyear,
        4 => TimeFormat::DecimalYear,
        5 => TimeFormat::Jd,
        6 => TimeFormat::Mjd,
        7 => TimeFormat::Gps,
        8 => TimeFormat::Unix,
        9 => TimeFormat::Utime,
        10 => TimeFormat::TaiSeconds,
        11 => TimeFormat::Cxcsec,
        12 => TimeFormat::Galexsec,
        13 => TimeFormat::UnixTai,
        14 => TimeFormat::Reserved1,
        15 => TimeFormat::ByearStr,
        16 => TimeFormat::Datetime,
        17 => TimeFormat::Fits,
        18 => TimeFormat::Isot,
        19 => TimeFormat::JyearStr,
        20 => TimeFormat::PlotDate,
        21 => TimeFormat::Ymdhms,
        22 => TimeFormat::Datetime64,
        _ => TimeFormat::Iso,
    }
}

impl TimeScale {
    /// The name written to a file.
    pub fn name(self) -> &'static str {
        SCALE_NAMES[self as usize]
    }

    /// Parse a scale name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "utc" => TimeScale::Utc,
            "tai" => TimeScale::Tai,
            "tcb" => TimeScale::Tcb,
            "tcg" => TimeScale::Tcg,
            "tdb" => TimeScale::Tdb,
            "tt" => TimeScale::Tt,
            "ut1" => TimeScale::Ut1,
            _ => return None,
        })
    }

    /// Convert from the ABI discriminant.
    pub fn from_i32(value: i32) -> Self {
        match value {
            1 => TimeScale::Tai,
            2 => TimeScale::Tcb,
            3 => TimeScale::Tcg,
            4 => TimeScale::Tdb,
            5 => TimeScale::Tt,
            6 => TimeScale::Ut1,
            _ => TimeScale::Utc,
        }
    }
}

/// An observer's location, for the location-sensitive scales.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Location {
    /// Degrees east.
    pub longitude: f64,
    /// Degrees north.
    pub latitude: f64,
    /// Metres above the reference ellipsoid.
    pub height: f64,
}

/// A calendar breakdown, in the format's own timescale.
///
/// Deliberately free of C types, so the engine stays platform-neutral; the
/// FFI layer converts this to `struct tm` and `struct timespec`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Civil {
    /// Full year, e.g. 2026.
    pub year: i32,
    /// Month, 1 to 12.
    pub month: u32,
    /// Day of the month, 1 to 31.
    pub day: u32,
    /// Hour, 0 to 23.
    pub hour: u32,
    /// Minute, 0 to 59.
    pub minute: u32,
    /// Second, 0 to 60 to allow for a leap second in the source text.
    pub second: u32,
    /// Nanoseconds within the second.
    pub nanosecond: u32,
    /// Day of the year, 1 to 366.
    pub yday: u32,
    /// Day of the week, 0 being Sunday.
    pub wday: u32,
    /// Seconds from the Unix epoch, ignoring leap seconds.
    pub unix_seconds: i64,
}

// Julian Dates of the epochs each numeric format counts from. Taken from
// astropy's `TimeFromEpoch` subclasses, as libasdf's are.
const JD_UNIX_EPOCH: f64 = 2440587.5;
const JD_MJD: f64 = 2400000.5;
const JD_J2000: f64 = 2451545.0;
const JD_B1900: f64 = 2415020.31352;
/// matplotlib counts days from 0001-01-01 UTC *plus one*.
const JD_PLOT_DATE_EPOCH: f64 = 1721424.5;
/// 1980-01-06 00:00:19 TAI.
const JD_GPS_EPOCH: f64 = 2444244.5 + 19.0 / 86400.0;
/// 1980-01-06 00:00:00 UTC.
const JD_GALEXSEC_EPOCH: f64 = 2444244.5;
/// 1998-01-01 00:00:00 TT.
const JD_CXCSEC_EPOCH: f64 = 2450814.5;
/// 1958-01-01 00:00:00 TAI.
const JD_TAI_SECONDS_EPOCH: f64 = 2436204.5;
/// 1979-01-01 00:00:00 UTC.
const JD_UTIME_EPOCH: f64 = 2443874.5;

const JULIAN_YEAR_DAYS: f64 = 365.25;
const BESSELIAN_YEAR_DAYS: f64 = 365.242198781;
const SECONDS_PER_DAY: f64 = 86400.0;

/// Days from 1970-01-01 for a civil date, by Howard Hinnant's algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    ((year + i64::from(month <= 2)) as i32, month, day)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Fill in the derived fields of a partially-built breakdown.
///
/// `unix_seconds` is computed in the **proleptic Gregorian** calendar, which
/// is what `timegm` gives libasdf and what a caller treating it as a Unix
/// timestamp expects. For a date before 1582-10-15 that disagrees with the
/// Julian-calendar breakdown by a growing number of days; such dates are far
/// outside what a Unix timestamp is meaningful for.
fn complete(mut civil: Civil) -> Civil {
    let days = days_from_civil(civil.year, civil.month.max(1), civil.day.max(1));
    civil.unix_seconds = days * 86_400
        + i64::from(civil.hour) * 3600
        + i64::from(civil.minute) * 60
        + i64::from(civil.second);

    // Day of the year.
    let month_lengths =
        [31, if is_leap(civil.year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut yday = civil.day;
    for length in month_lengths.iter().take(civil.month.saturating_sub(1) as usize) {
        yday += length;
    }
    civil.yday = yday;

    // 1970-01-01 was a Thursday, which is weekday 4 counting Sunday as 0.
    civil.wday = (((days % 7) + 7 + 4) % 7) as u32;
    civil
}

/// Convert a Julian Date to a calendar breakdown, by Meeus' algorithm.
pub fn julian_to_civil(jd: f64) -> Civil {
    let shifted = jd + 0.5;
    let z = shifted.floor();
    let fraction = shifted - z;

    // The Gregorian correction applies from 1582-10-15 onwards.
    let a = if z < 2299161.0 {
        z
    } else {
        let alpha = ((z - 1867216.25) / 36524.25).floor();
        z + 1.0 + alpha - (alpha / 4.0).floor()
    };
    let b = a + 1524.0;
    let c = ((b - 122.1) / 365.25).floor();
    let d = (365.25 * c).floor();
    let e = ((b - d) / 30.6001).floor();

    let day_with_fraction = b - d - (30.6001 * e).floor() + fraction;
    let day = day_with_fraction.floor();
    let month = if e < 14.0 { e - 1.0 } else { e - 13.0 };
    let year = if month > 2.0 { c - 4716.0 } else { c - 4715.0 };

    // Split the day fraction into a time of day, rounding to the nearest
    // nanosecond so a value that is exact in seconds does not drift.
    let seconds_in_day = (day_with_fraction - day) * SECONDS_PER_DAY;
    let total_nanos = (seconds_in_day * 1e9).round().max(0.0) as i64;
    let whole_seconds = total_nanos / 1_000_000_000;
    let nanosecond = (total_nanos % 1_000_000_000) as u32;

    complete(Civil {
        year: year as i32,
        month: month as u32,
        day: day as u32,
        hour: (whole_seconds / 3600) as u32,
        minute: ((whole_seconds / 60) % 60) as u32,
        second: (whole_seconds % 60) as u32,
        nanosecond,
        ..Default::default()
    })
}

/// The inverse of [`julian_to_civil`], by Meeus' algorithm.
///
/// Uses the same calendar convention as the forward direction -- Julian
/// before 1582-10-15, Gregorian from then on -- so the two are mutual
/// inverses across the whole range. Deriving this from
/// [`days_from_civil`] instead would be proleptic Gregorian and disagree
/// with the forward conversion for any date before the switch.
pub fn civil_to_julian(civil: &Civil) -> f64 {
    let (mut year, mut month) = (civil.year, civil.month as i32);
    if month <= 2 {
        year -= 1;
        month += 12;
    }

    // The Gregorian correction applies from 1582-10-15 onwards.
    let gregorian = (civil.year, civil.month, civil.day) >= (1582, 10, 15);
    let b = if gregorian {
        let a = (year as f64 / 100.0).floor();
        2.0 - a + (a / 4.0).floor()
    } else {
        0.0
    };

    let seconds = f64::from(civil.hour) * 3600.0
        + f64::from(civil.minute) * 60.0
        + f64::from(civil.second)
        + f64::from(civil.nanosecond) / 1e9;

    (365.25 * (f64::from(year) + 4716.0)).floor()
        + (30.6001 * (f64::from(month) + 1.0)).floor()
        + f64::from(civil.day)
        + b
        - 1524.5
        + seconds / SECONDS_PER_DAY
}

/// Split a trailing UTC offset off a time-of-day.
///
/// Returns the time without it and the offset in seconds. `Z` means zero,
/// and so does no designator at all -- ASDF carries the scale separately, so
/// an unqualified time is read in its own scale.
///
/// The forms are ISO 8601's: `+HH:MM`, `-HH:MM`, `+HHMM` and `+HH`.
fn split_utc_offset(time: &str) -> (&str, i64) {
    let time = time.trim();
    if let Some(rest) = time.strip_suffix(['Z', 'z']) {
        return (rest.trim_end(), 0);
    }
    // The sign cannot be the first character: that would be part of the time
    // itself, not an offset.
    let Some(index) = time.rfind(['+', '-']).filter(|i| *i > 0) else {
        return (time, 0);
    };
    let sign = if time.as_bytes()[index] == b'-' { -1 } else { 1 };
    let designator = &time[index + 1..];

    let (hours, minutes) = match designator.split_once(':') {
        Some((h, m)) => (h, m),
        // `+HHMM` packs both without a separator; `+HH` has only hours.
        None if designator.len() == 4 => designator.split_at(2),
        None => (designator, "0"),
    };
    let (Ok(hours), Ok(minutes)) = (hours.parse::<i64>(), minutes.parse::<i64>()) else {
        return (time, 0);
    };
    (&time[..index], sign * (hours * 3600 + minutes * 60))
}

/// Parse an ISO 8601 or FITS date-time.
///
/// Accepts `YYYY-MM-DD[T ]HH:MM:SS[.frac]`, with the time optional, and a
/// signed five-digit "long year" as FITS permits. A trailing UTC offset is
/// applied, so `11:56:15+01:00` is 10:56:15 UTC.
fn parse_datetime(text: &str) -> Option<Civil> {
    let text = text.trim();
    let (date, time) = match text.find(['T', ' ']) {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };

    let (negative, date) = match date.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, date.strip_prefix('+').unwrap_or(date)),
    };

    let mut parts = date.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next().unwrap_or("1").parse().ok()?;
    let day: u32 = parts.next().unwrap_or("1").parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut utc_offset = 0i64;
    let (hour, minute, second, nanosecond) = match time {
        None => (0, 0, 0, 0),
        Some(time) => {
            let (time, offset) = split_utc_offset(time);
            utc_offset = offset;
            let mut parts = time.split(':');
            let hour: u32 = parts.next()?.parse().ok()?;
            let minute: u32 = parts.next().unwrap_or("0").parse().ok()?;
            let seconds_text = parts.next().unwrap_or("0");

            let (whole, fraction) = match seconds_text.split_once('.') {
                Some((whole, fraction)) => (whole, fraction),
                None => (seconds_text, ""),
            };
            let second: u32 = whole.parse().ok()?;
            // Scale the fractional digits to nanoseconds.
            let mut nanos = 0u32;
            for (index, digit) in fraction.chars().take(9).enumerate() {
                let value = digit.to_digit(10)?;
                nanos += value * 10u32.pow(8 - index as u32);
            }
            // A leap second is 60, which the calendar arithmetic folds over.
            if hour > 23 || minute > 59 || second > 60 {
                return None;
            }
            (hour, minute, second, nanos)
        }
    };

    let civil = complete(Civil {
        year: if negative { -year } else { year },
        month,
        day,
        hour,
        minute,
        second,
        nanosecond,
        ..Default::default()
    });
    if utc_offset == 0 {
        return Some(civil);
    }
    // Re-derive the whole breakdown from the corrected instant rather than
    // adjusting `unix_seconds` alone, so the calendar fields and the
    // timestamp cannot disagree.
    Some(civil_from_unix_seconds(civil.unix_seconds - utc_offset, civil.nanosecond))
}

/// The calendar breakdown for an instant, in the proleptic Gregorian
/// calendar that [`complete`] uses.
fn civil_from_unix_seconds(unix_seconds: i64, nanosecond: u32) -> Civil {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    complete(Civil {
        year,
        month,
        day,
        hour: (seconds_of_day / 3600) as u32,
        minute: ((seconds_of_day % 3600) / 60) as u32,
        second: (seconds_of_day % 60) as u32,
        nanosecond,
        ..Default::default()
    })
}

/// Guess a time's format from the shape of its value string.
///
/// A `time/time` value need not say what format it is in, and the schema's
/// string forms are distinguishable, so the format is read off the value.
/// libasdf does this with five regexes tried in a fixed order; the order is
/// what makes an ordinary four-digit year ISO rather than FITS, since the
/// FITS pattern also admits one.
///
/// Returns `None` when the value matches none of them, which is an error at
/// the call site rather than a silent fall back to ISO.
pub fn infer_format(value: &str) -> Option<TimeFormat> {
    // Each pattern anchors at the start and may leave a tail, as libasdf's
    // do.
    if matches_iso_shape(value, false) {
        return Some(TimeFormat::Iso);
    }
    if let Some(rest) = value.strip_prefix('B')
        && matches_year_shape(rest)
    {
        return Some(TimeFormat::Byear);
    }
    if let Some(rest) = value.strip_prefix('J')
        && matches_year_shape(rest)
    {
        return Some(TimeFormat::Jyear);
    }
    if matches_yday_shape(value) {
        return Some(TimeFormat::Yday);
    }
    // Reached only for the signed five-digit "long year", since the plain
    // four-digit form was already claimed by ISO above.
    if matches_iso_shape(value, true) {
        return Some(TimeFormat::Fits);
    }
    None
}

/// `\d{4}-\d\d-\d\d([T ]\d\d:\d\d:\d\d(.\d+)?)?`, optionally
/// allowing FITS's signed five-digit year.
fn matches_iso_shape(value: &str, long_year: bool) -> bool {
    let bytes = value.as_bytes();
    let digits = |at: usize, count: usize| -> bool {
        bytes.len() >= at + count && bytes[at..at + count].iter().all(u8::is_ascii_digit)
    };

    let mut at = if long_year {
        // Only the signed five-digit branch: the four-digit one is ISO's.
        if !matches!(bytes.first(), Some(b'+' | b'-')) || !digits(1, 5) {
            return false;
        }
        6
    } else {
        if !digits(0, 4) {
            return false;
        }
        4
    };

    for _ in 0..2 {
        if bytes.get(at) != Some(&b'-') || !digits(at + 1, 2) {
            return false;
        }
        at += 3;
    }

    // The time of day is optional; anything else trailing is ignored, as
    // these are prefix matches.
    if !matches!(bytes.get(at), Some(b'T' | b' ')) {
        return true;
    }
    at += 1;
    if !digits(at, 2) {
        return false;
    }
    at += 2;
    for _ in 0..2 {
        if bytes.get(at) != Some(&b':') || !digits(at + 1, 2) {
            return false;
        }
        at += 3;
    }
    true
}

/// `\d+(.\d+)?`, the tail of a `B`/`J` epoch-year value.
fn matches_year_shape(value: &str) -> bool {
    let mut chars = value.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    true
}

/// `\d{4}:\d{3}:\d\d:\d\d:\d\d(.\d+)?`.
fn matches_yday_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digits = |at: usize, count: usize| -> bool {
        bytes.len() >= at + count && bytes[at..at + count].iter().all(u8::is_ascii_digit)
    };

    if !digits(0, 4) || bytes.get(4) != Some(&b':') {
        return false;
    }
    if !digits(5, 3) || bytes.get(8) != Some(&b':') {
        return false;
    }
    let mut at = 9;
    for step in 0..3 {
        if !digits(at, 2) {
            return false;
        }
        at += 2;
        if step < 2 {
            if bytes.get(at) != Some(&b':') {
                return false;
            }
            at += 1;
        }
    }
    true
}

/// Parse a `YYYY:DDD:HH:MM:SS` day-of-year time.
fn parse_yday(text: &str) -> Option<Civil> {
    let mut parts = text.trim().split(':');
    let year: i32 = parts.next()?.parse().ok()?;
    let yday: u32 = parts.next()?.parse().ok()?;
    let hour: u32 = parts.next().unwrap_or("0").parse().ok()?;
    let minute: u32 = parts.next().unwrap_or("0").parse().ok()?;
    let seconds_text = parts.next().unwrap_or("0");
    let (whole, fraction) = match seconds_text.split_once('.') {
        Some(split) => split,
        None => (seconds_text, ""),
    };
    let second: u32 = whole.parse().ok()?;
    let mut nanosecond = 0u32;
    for (index, digit) in fraction.chars().take(9).enumerate() {
        nanosecond += digit.to_digit(10)? * 10u32.pow(8 - index as u32);
    }

    if yday == 0 || yday > if is_leap(year) { 366 } else { 365 } {
        return None;
    }
    // Turn the day-of-year back into a month and day.
    let days = days_from_civil(year, 1, 1) + i64::from(yday) - 1;
    let (year, month, day) = civil_from_days(days);

    Some(complete(Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
        nanosecond,
        ..Default::default()
    }))
}

/// How YAML reads a scalar, which decides whether its format may be guessed.
fn scalar_kind(doc: &Document, node: NodeId) -> Resolved {
    match &doc.resolved(node).data {
        NodeData::Scalar { value, style } => {
            asdf_yaml::resolve(value, *style, asdf_yaml::Schema::Libasdf)
        }
        _ => Resolved::Null,
    }
}

/// Strip the `B`/`J` prefix from an epoch-year string.
fn parse_epoch_year(text: &str) -> Option<f64> {
    let text = text.trim();
    let body = text.strip_prefix('B').or_else(|| text.strip_prefix('J')).unwrap_or(text);
    body.parse().ok()
}

/// A time as it appears in a file.
#[derive(Clone, PartialEq, Debug)]
pub struct Time {
    /// The value exactly as written.
    pub value: String,
    /// The effective format, with `format` and `base_format` collapsed.
    pub format: TimeFormat,
    /// The time scale.
    pub scale: TimeScale,
    /// The observer's location, for location-sensitive scales.
    pub location: Location,
    /// The derived calendar breakdown, once computed.
    pub civil: Option<Civil>,
}

impl Time {
    /// A time with the given value and format.
    pub fn new(value: impl Into<String>, format: TimeFormat, scale: TimeScale) -> Self {
        Self { value: value.into(), format, scale, location: Location::default(), civil: None }
    }

    /// Read a `time/time` value from the tree.
    ///
    /// The schema allows two shapes: the whole tagged value as a string, or
    /// a mapping with the string under `value` and optional `format`,
    /// `base_format`, `scale` and `location`.
    ///
    /// The *wire* format says how to read the value; `base_format` records
    /// the object's real format and overrides only what it reports
    /// afterwards. An astropy `plot_date` is stored as an ISO string with
    /// `base_format: plot_date`, so collapsing the two before parsing would
    /// try to read a date as a matplotlib ordinal.
    ///
    /// The calendar breakdown is computed where the value allows it; a value
    /// that will not parse is not an error, since the value, format and
    /// scale round-trip regardless and the breakdown is a convenience.
    pub fn parse(doc: &Document, id: NodeId) -> Result<Self> {
        let node = doc.resolved(id);
        let mapping = node.is_mapping();

        let value_node = if mapping {
            let Some(found) = doc.mapping_get(id, "value") else {
                return Err(err!(InvalidArgument, "a time mapping needs a 'value'"));
            };
            doc.resolve(found)
        } else {
            doc.resolve(id)
        };

        // The raw text is what the format parsers want, even where YAML
        // would read it as a number.
        let Some(value) = doc.resolved(value_node).as_str().map(str::to_string) else {
            return Err(err!(InvalidArgument, "a time's value must be a scalar"));
        };
        // Whether YAML read it *as* a string decides what may be guessed: a
        // bare number is ambiguous and cannot be.
        let value_is_string = matches!(scalar_kind(doc, value_node), Resolved::String);

        let field = |key: &str| -> Option<String> {
            doc.mapping_get(id, key).and_then(|n| doc.resolved(n).as_str().map(str::to_string))
        };

        let (explicit, base, scale, location) = if mapping {
            let scale = field("scale")
                .and_then(|name| TimeScale::from_name(&name))
                .unwrap_or(TimeScale::Utc);

            let mut location = Location::default();
            if let Some(loc) = doc.mapping_get(id, "location") {
                let number = |key: &str| {
                    doc.mapping_get(loc, key)
                        .and_then(|n| doc.resolved(n).as_str())
                        .and_then(|text| text.parse::<f64>().ok())
                        .unwrap_or(0.0)
                };
                location.longitude = number("longitude");
                location.latitude = number("latitude");
                location.height = number("height");
            }
            (field("format"), field("base_format"), scale, location)
        } else {
            (None, None, TimeScale::Utc, Location::default())
        };

        let wire = match &explicit {
            Some(name) => TimeFormat::from_name(name)
                .ok_or_else(|| err!(InvalidArgument, "unknown time format {name:?}"))?,
            None => {
                if !value_is_string {
                    return Err(err!(
                        InvalidArgument,
                        "a numeric time value needs an explicit format; {value:?} is ambiguous"
                    ));
                }
                infer_format(&value).ok_or_else(|| {
                    err!(InvalidArgument, "could not guess the format of time {value:?}")
                })?
            }
        };

        // `jyear_str` and `byear_str` exist to make the `J`/`B` prefix
        // mandatory, so a bare number under either is not a time.
        if matches!(wire, TimeFormat::JyearStr | TimeFormat::ByearStr) {
            let prefix = if wire == TimeFormat::JyearStr { ['J', 'j'] } else { ['B', 'b'] };
            if !value_is_string || !value.starts_with(prefix) {
                return Err(err!(
                    InvalidArgument,
                    "time format {:?} needs a value starting with {:?}",
                    wire.name(),
                    prefix[0]
                ));
            }
        }

        // An unrecognised `base_format` is a label we do not know; the value
        // is still readable without it.
        let effective = base.as_deref().and_then(TimeFormat::from_name).unwrap_or(wire);

        let mut time = Time::new(value, wire, scale);
        time.location = location;
        let civil = time.compute_civil().ok();
        Ok(Time { format: effective, civil, ..time })
    }

    /// Compute the calendar breakdown from the value, format and scale.
    ///
    /// Approximate for anything off the UTC scale; see the module comment.
    pub fn compute_civil(&mut self) -> Result<Civil> {
        let civil = self.derive_civil()?;
        self.civil = Some(civil);
        Ok(civil)
    }

    fn derive_civil(&self) -> Result<Civil> {
        let text = self.value.trim();
        let numeric = || -> Result<f64> {
            text.parse::<f64>()
                .map_err(|_| err!(InvalidArgument, "time value {text:?} is not numeric"))
        };

        let civil = match self.format {
            // String forms are parsed directly.
            TimeFormat::Iso
            | TimeFormat::Isot
            | TimeFormat::Fits
            | TimeFormat::Datetime
            | TimeFormat::Datetime64
            | TimeFormat::Ymdhms => parse_datetime(text)
                .ok_or_else(|| err!(InvalidArgument, "could not parse {text:?} as a date-time"))?,

            TimeFormat::Yday => parse_yday(text)
                .ok_or_else(|| err!(InvalidArgument, "could not parse {text:?} as a yday time"))?,

            // Julian and modified Julian dates convert directly.
            TimeFormat::Jd => julian_to_civil(numeric()?),
            TimeFormat::Mjd => julian_to_civil(numeric()? + JD_MJD),

            // Epoch years.
            TimeFormat::Jyear | TimeFormat::JyearStr => {
                let year = parse_epoch_year(text)
                    .ok_or_else(|| err!(InvalidArgument, "bad Julian epoch {text:?}"))?;
                julian_to_civil(JD_J2000 + JULIAN_YEAR_DAYS * (year - 2000.0))
            }
            TimeFormat::Byear | TimeFormat::ByearStr => {
                let year = parse_epoch_year(text)
                    .ok_or_else(|| err!(InvalidArgument, "bad Besselian epoch {text:?}"))?;
                julian_to_civil(JD_B1900 + BESSELIAN_YEAR_DAYS * (year - 1900.0))
            }
            TimeFormat::DecimalYear => {
                let year = numeric()?;
                let whole = year.floor();
                let days_in_year = if is_leap(whole as i32) { 366.0 } else { 365.0 };
                let start = days_from_civil(whole as i32, 1, 1) as f64;
                julian_to_civil(JD_UNIX_EPOCH + start + (year - whole) * days_in_year)
            }

            // matplotlib's ordinal.
            TimeFormat::PlotDate => julian_to_civil(numeric()? + JD_PLOT_DATE_EPOCH),

            // Seconds from an epoch.
            TimeFormat::Unix => julian_to_civil(JD_UNIX_EPOCH + numeric()? / SECONDS_PER_DAY),
            TimeFormat::UnixTai => julian_to_civil(JD_UNIX_EPOCH + numeric()? / SECONDS_PER_DAY),
            TimeFormat::Gps => julian_to_civil(JD_GPS_EPOCH + numeric()? / SECONDS_PER_DAY),
            TimeFormat::Galexsec => {
                julian_to_civil(JD_GALEXSEC_EPOCH + numeric()? / SECONDS_PER_DAY)
            }
            TimeFormat::Cxcsec => julian_to_civil(JD_CXCSEC_EPOCH + numeric()? / SECONDS_PER_DAY),
            TimeFormat::TaiSeconds => {
                julian_to_civil(JD_TAI_SECONDS_EPOCH + numeric()? / SECONDS_PER_DAY)
            }
            TimeFormat::Utime => julian_to_civil(JD_UTIME_EPOCH + numeric()? / SECONDS_PER_DAY),

            TimeFormat::Reserved1 => {
                return Err(err!(InvalidArgument, "the reserved time format is not usable"));
            }
        };
        Ok(civil)
    }

    /// The pair of values to write: the wire `format`, and `base_format`
    /// when the effective format may not appear in `format`.
    pub fn wire_formats(&self) -> (TimeFormat, Option<TimeFormat>) {
        if self.format.is_other() {
            (self.format.standard(), Some(self.format))
        } else {
            (self.format, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_discriminants_match_the_c_abi() {
        assert_eq!(TimeFormat::Iso as i32, 0);
        assert_eq!(TimeFormat::Yday as i32, 1);
        assert_eq!(TimeFormat::UnixTai as i32, 13);
        assert_eq!(TimeFormat::Reserved1 as i32, 14);
        assert_eq!(TimeFormat::ByearStr as i32, 15);
        assert_eq!(TimeFormat::Datetime64 as i32, 22);

        assert_eq!(TimeScale::Utc as i32, 0);
        assert_eq!(TimeScale::Ut1 as i32, 6);
    }

    #[test]
    fn format_names_round_trip() {
        for index in 0..23 {
            let format = TimeFormat::from_index(index).unwrap();
            match format.name() {
                Some(name) => assert_eq!(TimeFormat::from_name(name), Some(format), "{name}"),
                // Only the reserved slot has no name, as upstream.
                None => assert_eq!(format, TimeFormat::Reserved1),
            }
        }
        assert_eq!(TimeFormat::from_name("nonsense"), None);
    }

    #[test]
    fn scale_names_round_trip() {
        for scale in [
            TimeScale::Utc,
            TimeScale::Tai,
            TimeScale::Tcb,
            TimeScale::Tcg,
            TimeScale::Tdb,
            TimeScale::Tt,
            TimeScale::Ut1,
        ] {
            assert_eq!(TimeScale::from_name(scale.name()), Some(scale));
            assert_eq!(TimeScale::from_i32(scale as i32), scale);
        }
    }

    #[test]
    fn other_formats_split_into_base_format() {
        // The schema only permits a subset in `format`; the rest go in
        // `base_format` with a standard stand-in.
        for (other, standard) in [
            (TimeFormat::Isot, TimeFormat::Iso),
            (TimeFormat::Fits, TimeFormat::Iso),
            (TimeFormat::PlotDate, TimeFormat::Iso),
            (TimeFormat::Ymdhms, TimeFormat::Iso),
            (TimeFormat::Datetime64, TimeFormat::Iso),
            (TimeFormat::JyearStr, TimeFormat::Jyear),
            (TimeFormat::ByearStr, TimeFormat::Byear),
        ] {
            assert!(other.is_other(), "{other:?}");
            assert_eq!(other.standard(), standard);

            let time = Time::new("x", other, TimeScale::Utc);
            assert_eq!(time.wire_formats(), (standard, Some(other)));
        }

        // A standard format needs no base_format.
        let time = Time::new("2026-01-01", TimeFormat::Iso, TimeScale::Utc);
        assert_eq!(time.wire_formats(), (TimeFormat::Iso, None));
        assert!(!TimeFormat::Iso.is_other());
    }

    #[test]
    fn civil_day_arithmetic_round_trips() {
        for (year, month, day) in
            [(1970, 1, 1), (2000, 2, 29), (1999, 12, 31), (2026, 9, 4), (1582, 10, 15), (1, 1, 1)]
        {
            let days = days_from_civil(year, month, day);
            assert_eq!(civil_from_days(days), (year, month, day), "{year}-{month}-{day}");
        }
        // The Unix epoch is day zero, by definition.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    /// A trailing UTC offset shifts the instant, as ISO 8601 says and as
    /// libasdf's own parser does.
    #[test]
    fn utc_offsets_are_applied() {
        // The exact value upstream's `test-core-extensions` expects from
        // `fixtures/255.asdf`.
        let mut t = Time::new("2025-07-23 11:56:15+00:00", TimeFormat::Iso, TimeScale::Utc);
        assert_eq!(t.compute_civil().unwrap().unix_seconds, 1_753_271_775);

        // An hour east is an hour earlier in UTC.
        let mut east = Time::new("2025-07-23T11:56:15+01:00", TimeFormat::Iso, TimeScale::Utc);
        assert_eq!(east.compute_civil().unwrap().unix_seconds, 1_753_271_775 - 3600);

        // And an hour west is an hour later.
        let mut west = Time::new("2025-07-23T11:56:15-01:00", TimeFormat::Iso, TimeScale::Utc);
        assert_eq!(west.compute_civil().unwrap().unix_seconds, 1_753_271_775 + 3600);

        // The calendar fields follow the instant, not the written text.
        let shifted = east.compute_civil().unwrap();
        assert_eq!((shifted.hour, shifted.minute, shifted.second), (10, 56, 15));

        // `Z` and a bare time both mean no offset.
        for text in ["2025-07-23T11:56:15Z", "2025-07-23T11:56:15"] {
            let mut t = Time::new(text, TimeFormat::Iso, TimeScale::Utc);
            assert_eq!(t.compute_civil().unwrap().unix_seconds, 1_753_271_775, "{text}");
        }
    }

    #[test]
    fn offset_designators_come_in_several_shapes() {
        for (text, expected) in [
            ("2025-07-23T11:56:15+01:30", 1_753_271_775 - 5400),
            ("2025-07-23T11:56:15+0130", 1_753_271_775 - 5400),
            ("2025-07-23T11:56:15+01", 1_753_271_775 - 3600),
            ("2025-07-23T11:56:15-0130", 1_753_271_775 + 5400),
        ] {
            let mut t = Time::new(text, TimeFormat::Iso, TimeScale::Utc);
            assert_eq!(t.compute_civil().unwrap().unix_seconds, expected, "{text}");
        }
    }

    /// A negative year's leading sign must not be read as an offset.
    #[test]
    fn a_negative_year_is_not_an_offset() {
        let mut t = Time::new("-0044-03-15T12:00:00", TimeFormat::Iso, TimeScale::Utc);
        let civil = t.compute_civil().unwrap();
        assert_eq!(civil.year, -44);
        assert_eq!((civil.month, civil.day, civil.hour), (3, 15, 12));
    }

    /// The fixture upstream's `test-core-extensions` reads, whose time is a
    /// single-quoted scalar folded across two lines.
    #[test]
    fn a_folded_time_string_still_parses() {
        let doc = asdf_yaml::parse_document("time: '2025-07-23\n      11:56:15+00:00'\n").unwrap();
        let root = doc.root().unwrap();
        let node = doc.mapping_get(root, "time").unwrap();
        let text = doc.resolved(node).as_str().unwrap();
        assert_eq!(text, "2025-07-23 11:56:15+00:00", "the fold should become one space");

        let mut t = Time::new(text, TimeFormat::Iso, TimeScale::Utc);
        assert_eq!(t.compute_civil().unwrap().unix_seconds, 1_753_271_775);
    }

    /// The shapes libasdf's five auto-detect patterns match, in its order.
    #[test]
    fn formats_are_inferred_from_the_value_string() {
        use TimeFormat as F;
        let cases = [
            ("2025-10-14T13:26:41.0000", Some(F::Iso)),
            ("2025-10-14 13:26:41", Some(F::Iso)),
            ("2025-10-14", Some(F::Iso)),
            ("B2025.78707178", Some(F::Byear)),
            ("J2025.78707178", Some(F::Jyear)),
            ("2025:287:13:26:41.0000", Some(F::Yday)),
            // The signed five-digit "long year" is the only thing that
            // reaches the FITS pattern; a four-digit year is ISO first.
            ("+12025-10-14T13:26:41.0000", Some(F::Fits)),
            ("-12025-10-14T13:26:41.0000", Some(F::Fits)),
            ("not a time at all", None),
            ("2025-13", None),
            ("B", None),
            ("2025:287", None),
        ];
        for (text, expected) in cases {
            assert_eq!(infer_format(text), expected, "{text}");
        }
    }

    #[test]
    fn parses_iso_times() {
        let mut time = Time::new("2026-09-04T12:34:56.5", TimeFormat::Iso, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!((civil.year, civil.month, civil.day), (2026, 9, 4));
        assert_eq!((civil.hour, civil.minute, civil.second), (12, 34, 56));
        assert_eq!(civil.nanosecond, 500_000_000);
    }

    #[test]
    fn a_date_without_a_time_is_midnight() {
        let mut time = Time::new("2026-09-04", TimeFormat::Iso, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!((civil.hour, civil.minute, civil.second), (0, 0, 0));
        assert_eq!(civil.unix_seconds, days_from_civil(2026, 9, 4) * 86_400);
    }

    #[test]
    fn the_unix_epoch_is_the_anchor() {
        let mut time = Time::new("1970-01-01T00:00:00", TimeFormat::Iso, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!(civil.unix_seconds, 0);
        // 1970-01-01 was a Thursday.
        assert_eq!(civil.wday, 4);
        assert_eq!(civil.yday, 1);
    }

    #[test]
    fn julian_dates_convert_both_ways() {
        // J2000.0 is 2000-01-01 12:00 TT, JD 2451545.0.
        let civil = julian_to_civil(JD_J2000);
        assert_eq!((civil.year, civil.month, civil.day), (2000, 1, 1));
        assert_eq!(civil.hour, 12);

        // And back again.
        let back = civil_to_julian(&civil);
        assert!((back - JD_J2000).abs() < 1e-6, "{back} != {JD_J2000}");
    }

    #[test]
    fn numeric_formats_land_on_their_epochs() {
        // Each format's zero must be its documented epoch instant.
        let cases = [
            (TimeFormat::Unix, "0", (1970, 1, 1)),
            (TimeFormat::Galexsec, "0", (1980, 1, 6)),
            (TimeFormat::Cxcsec, "0", (1998, 1, 1)),
            (TimeFormat::TaiSeconds, "0", (1958, 1, 1)),
            (TimeFormat::Utime, "0", (1979, 1, 1)),
            (TimeFormat::Mjd, "0", (1858, 11, 17)),
        ];
        for (format, value, expected) in cases {
            let mut time = Time::new(value, format, TimeScale::Utc);
            let civil = time.compute_civil().unwrap();
            assert_eq!((civil.year, civil.month, civil.day), expected, "{format:?} epoch");
        }
    }

    #[test]
    fn unix_seconds_are_recovered_from_a_unix_time() {
        // A known instant: 2026-09-04T00:00:00Z.
        let seconds = days_from_civil(2026, 9, 4) * 86_400;
        let mut time = Time::new(seconds.to_string(), TimeFormat::Unix, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!((civil.year, civil.month, civil.day), (2026, 9, 4));
        assert_eq!(civil.unix_seconds, seconds);
    }

    #[test]
    fn epoch_year_formats_parse_their_prefixes() {
        // J2000.0 is the Julian epoch's anchor.
        let mut time = Time::new("J2000.0", TimeFormat::JyearStr, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!((civil.year, civil.month, civil.day), (2000, 1, 1));

        // B1950.0 is the classic Besselian epoch.
        let mut time = Time::new("B1950.0", TimeFormat::ByearStr, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!(civil.year, 1949, "B1950.0 falls in late 1949");
        assert_eq!(civil.month, 12);

        // The bare numeric forms work too.
        let mut time = Time::new("2000.0", TimeFormat::Jyear, TimeScale::Utc);
        assert_eq!(time.compute_civil().unwrap().year, 2000);
    }

    #[test]
    fn yday_times_parse() {
        // 2026 is not a leap year, so day 247 is 4 September.
        let yday = days_from_civil(2026, 9, 4) - days_from_civil(2026, 1, 1) + 1;
        let mut time =
            Time::new(format!("2026:{yday:03}:12:00:00"), TimeFormat::Yday, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!((civil.year, civil.month, civil.day), (2026, 9, 4));
        assert_eq!(civil.hour, 12);
        assert_eq!(civil.yday, yday as u32);
    }

    #[test]
    fn a_leap_year_february_has_29_days() {
        let mut time = Time::new("2000-02-29T00:00:00", TimeFormat::Iso, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!(civil.day, 29);
        assert_eq!(civil.yday, 60);
        assert!(is_leap(2000));
        assert!(!is_leap(1900), "1900 is not a leap year");
        assert!(is_leap(2024));
    }

    #[test]
    fn fits_long_years_and_negatives_parse() {
        let mut time = Time::new("-0500-01-01T00:00:00", TimeFormat::Fits, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!(civil.year, -500);
    }

    #[test]
    fn a_leap_second_is_accepted() {
        // 60 appears in real UTC timestamps; it must parse rather than fail.
        let mut time = Time::new("2016-12-31T23:59:60", TimeFormat::Iso, TimeScale::Utc);
        assert!(time.compute_civil().is_ok());
    }

    #[test]
    fn malformed_values_are_errors_not_panics() {
        for (value, format) in [
            ("not a date", TimeFormat::Iso),
            ("2026-13-45", TimeFormat::Iso),
            ("2026-01-01T25:00:00", TimeFormat::Iso),
            ("not a number", TimeFormat::Unix),
            ("", TimeFormat::Iso),
            ("2026:400:00:00:00", TimeFormat::Yday),
        ] {
            let mut time = Time::new(value, format, TimeScale::Utc);
            assert!(time.compute_civil().is_err(), "{value:?} as {format:?}");
        }
    }

    #[test]
    fn the_reserved_format_is_refused() {
        let mut time = Time::new("0", TimeFormat::Reserved1, TimeScale::Utc);
        assert!(time.compute_civil().is_err());
        assert_eq!(TimeFormat::Reserved1.name(), None);
    }

    /// matplotlib's ordinal counts days from 0001-01-01 *plus one*, so
    /// `1.0` is that date in the proleptic Gregorian calendar.
    ///
    /// The breakdown reports 0001-01-03 rather than 0001-01-01, and that is
    /// correct rather than an off-by-two. Meeus' algorithm -- which libasdf
    /// uses too -- switches to the **Julian** calendar for instants before
    /// 1582-10-15, and proleptic-Gregorian 0001-01-01 is Julian 0001-01-03.
    /// A date after the switch has no such ambiguity, as the `mjd` epoch
    /// case above shows.
    #[test]
    fn plot_date_counts_from_its_own_epoch() {
        let mut time = Time::new("1.0", TimeFormat::PlotDate, TimeScale::Utc);
        let civil = time.compute_civil().unwrap();
        assert_eq!((civil.year, civil.month, civil.day), (1, 1, 3));

        // The two conversions are mutual inverses even here, because both
        // use the same calendar convention.
        let jd = civil_to_julian(&civil);
        let back = julian_to_civil(jd);
        assert_eq!((back.year, back.month, back.day), (1, 1, 3));
    }

    /// The two Julian Date conversions must invert each other across the
    /// whole range, including either side of the calendar switch.
    #[test]
    fn julian_conversions_invert_each_other() {
        // A sweep of Julian Dates spanning year 1 to well past 2100.
        let mut jd = 1_721_400.5;
        let mut checked = 0;
        while jd < 2_500_000.5 {
            let civil = julian_to_civil(jd);
            let back = civil_to_julian(&civil);
            assert!((back - jd).abs() < 1e-6, "JD {jd} became {civil:?} and back to {back}");

            // And the calendar breakdown itself must be self-consistent.
            let again = julian_to_civil(back);
            assert_eq!(
                (again.year, again.month, again.day, again.hour),
                (civil.year, civil.month, civil.day, civil.hour),
                "JD {jd} did not survive two conversions"
            );
            checked += 1;
            jd += 977.0; // a prime stride, so months and years vary
        }
        assert!(checked > 700, "expected a wide sweep, got {checked}");
    }

    /// Dates after the Gregorian switch are unambiguous, which is where the
    /// calendar boundary itself can be checked.
    #[test]
    fn the_gregorian_switch_is_where_meeus_puts_it() {
        // 1582-10-15 is the first Gregorian day; JD 2299160.5 is its start.
        let civil = julian_to_civil(2299160.5);
        assert_eq!((civil.year, civil.month, civil.day), (1582, 10, 15));

        // The day before it, in the Julian calendar, is 1582-10-04.
        let civil = julian_to_civil(2299159.5);
        assert_eq!((civil.year, civil.month, civil.day), (1582, 10, 4));
    }
}

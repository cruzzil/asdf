//! `asdf/core/time.h`.
//!
//! `asdf_time_t` is a public struct that embeds `struct timespec` and
//! `struct tm` — platform types, which is why they come from `libc` rather
//! than being hand-rolled. Their layouts differ between targets, so the
//! layout gate checks this one on every platform in the matrix.

use std::ffi::{CStr, CString, c_char, c_int};

use asdf_core::core::time::{Civil, Time, TimeFormat, TimeScale, infer_format};
use asdf_core::yaml::{Document, NodeId};

use crate::panic::guard;
use crate::types::AsdfValueErr;

/// `asdf_time_format_t` as it crosses the boundary.
pub type TimeFormatAbi = c_int;
/// `asdf_time_scale_t` as it crosses the boundary.
pub type TimeScaleAbi = c_int;

/// Mirror of `asdf_time_location_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct asdf_time_location_t {
    /// Degrees east.
    pub longitude: f64,
    /// Degrees north.
    pub latitude: f64,
    /// Metres above the reference ellipsoid.
    pub height: f64,
}

/// Mirror of `asdf_time_info_t`.
///
/// A derived, best-effort calendar reading. For anything off the UTC scale it
/// ignores leap seconds, because this library carries no leap-second table —
/// the same caveat libasdf documents.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct asdf_time_info_t {
    /// Seconds and nanoseconds from the Unix epoch.
    pub ts: libc::timespec,
    /// The broken-down calendar fields.
    pub tm: libc::tm,
}

impl std::fmt::Debug for asdf_time_info_t {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("asdf_time_info_t")
            .field("tv_sec", &self.ts.tv_sec)
            .field("tv_nsec", &self.ts.tv_nsec)
            .field("tm_year", &self.tm.tm_year)
            .finish()
    }
}

impl Default for asdf_time_info_t {
    fn default() -> Self {
        // SAFETY: both are plain C structs of integers and pointers, for
        // which an all-zero value is valid and is what C's `= {0}` gives.
        unsafe { std::mem::zeroed() }
    }
}

/// Mirror of `asdf_time_t`.
#[repr(C)]
#[derive(Debug)]
pub struct asdf_time_t {
    /// The value exactly as it appears in the file. Owned.
    pub value: *mut c_char,
    /// The derived calendar reading.
    pub info: asdf_time_info_t,
    /// The effective format, with `format` and `base_format` collapsed.
    pub format: TimeFormatAbi,
    /// The time scale.
    pub scale: TimeScaleAbi,
    /// The observer's location.
    pub location: asdf_time_location_t,
}

impl asdf_time_t {
    /// A zeroed instance.
    pub(crate) fn zeroed() -> Self {
        Self {
            value: std::ptr::null_mut(),
            info: asdf_time_info_t::default(),
            format: TimeFormat::Iso as c_int,
            scale: TimeScale::Utc as c_int,
            location: asdf_time_location_t::default(),
        }
    }
}

/// Fill a `struct tm` and `struct timespec` from a calendar breakdown.
fn fill_info(civil: &Civil) -> asdf_time_info_t {
    let mut info = asdf_time_info_t::default();
    info.ts.tv_sec = civil.unix_seconds as libc::time_t;
    info.ts.tv_nsec = libc::c_long::from(0) + i64::from(civil.nanosecond) as libc::c_long;

    // `struct tm` counts years from 1900 and months from zero.
    info.tm.tm_year = civil.year - 1900;
    info.tm.tm_mon = civil.month as c_int - 1;
    info.tm.tm_mday = civil.day as c_int;
    info.tm.tm_hour = civil.hour as c_int;
    info.tm.tm_min = civil.minute as c_int;
    info.tm.tm_sec = civil.second as c_int;
    info.tm.tm_yday = civil.yday as c_int - 1;
    info.tm.tm_wday = civil.wday as c_int;
    // These times carry their scale separately, so there is no DST notion.
    info.tm.tm_isdst = 0;
    info
}

/// Build the engine's view from a C struct.
fn to_engine(time: &asdf_time_t) -> Option<Time> {
    if time.value.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(time.value) }.to_string_lossy().into_owned();
    Some(Time {
        value,
        format: format_from_abi(time.format),
        scale: TimeScale::from_i32(time.scale),
        location: asdf_core::core::time::Location {
            longitude: time.location.longitude,
            latitude: time.location.latitude,
            height: time.location.height,
        },
        civil: None,
    })
}

fn format_from_abi(value: TimeFormatAbi) -> TimeFormat {
    TimeFormat::from_name(
        FORMAT_LOOKUP
            .get(usize::try_from(value).unwrap_or(usize::MAX))
            .copied()
            .flatten()
            .unwrap_or("iso"),
    )
    .unwrap_or(TimeFormat::Iso)
}

/// Format names by discriminant, for converting an ABI value back.
const FORMAT_LOOKUP: [Option<&str>; 23] = [
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

/// Compute a time's calendar reading from its value, format and scale.
///
/// Returns 0 on success and non-zero when the value cannot be interpreted.
///
/// # Safety
/// `time` must be null or a valid `asdf_time_t` with a `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn asdf_time_parse(time: *mut asdf_time_t) -> c_int {
    guard("asdf_time_parse", -1, || {
        if time.is_null() {
            return -1;
        }
        let handle = unsafe { &mut *time };
        let Some(mut engine) = to_engine(handle) else {
            return -1;
        };
        match engine.compute_civil() {
            Ok(civil) => {
                handle.info = fill_info(&civil);
                0
            }
            Err(_) => -1,
        }
    })
}

/// The name of a time format, or null for the reserved slot.
///
/// # Safety
/// The returned pointer refers to a `'static` string.
#[unsafe(no_mangle)]
pub extern "C" fn asdf_time_format_string(format: TimeFormatAbi) -> *const c_char {
    // Static names, so no allocation and no lifetime question.
    let name: Option<&'static CStr> = match format {
        0 => Some(c"iso"),
        1 => Some(c"yday"),
        2 => Some(c"byear"),
        3 => Some(c"jyear"),
        4 => Some(c"decimalyear"),
        5 => Some(c"jd"),
        6 => Some(c"mjd"),
        7 => Some(c"gps"),
        8 => Some(c"unix"),
        9 => Some(c"utime"),
        10 => Some(c"tai_seconds"),
        11 => Some(c"cxcsec"),
        12 => Some(c"galexsec"),
        13 => Some(c"unix_tai"),
        // The reserved slot has no name, matching upstream's NULL entry.
        14 => None,
        15 => Some(c"byear_str"),
        16 => Some(c"datetime"),
        17 => Some(c"fits"),
        18 => Some(c"isot"),
        19 => Some(c"jyear_str"),
        20 => Some(c"plot_date"),
        21 => Some(c"ymdhms"),
        22 => Some(c"datetime64"),
        _ => None,
    };
    name.map_or(std::ptr::null(), CStr::as_ptr)
}

/// The tag for `core/time`.
pub const TIME_TAG: &str = "tag:stsci.edu:asdf/time/time-1.4.0";

/// Read a `time/time` value from the tree.
pub(crate) fn time_deserialize(
    doc: &Document,
    node: NodeId,
    _file: *mut crate::file_ffi::AsdfFile,
    out: *mut asdf_time_t,
) -> AsdfValueErr {
    // The reading is the engine's; this only lays the result out for C.
    let Ok(time) = Time::parse(doc, node) else {
        return AsdfValueErr::ParseFailure;
    };
    let Ok(owned) = CString::new(time.value.clone()) else {
        return AsdfValueErr::ParseFailure;
    };

    unsafe {
        (*out).value = owned.into_raw();
        (*out).format = time.format as c_int;
        (*out).scale = time.scale as c_int;
        (*out).location = asdf_time_location_t {
            longitude: time.location.longitude,
            latitude: time.location.latitude,
            height: time.location.height,
        };
        // A value that will not parse is not an error: the value, format and
        // scale are authoritative and round-trip regardless, and the
        // breakdown is only a convenience.
        (*out).info = match time.civil {
            Some(civil) => fill_info(&civil),
            None => asdf_time_info_t::default(),
        };
    }
    AsdfValueErr::Ok
}

pub(crate) fn time_serialize(doc: &mut Document, obj: &asdf_time_t) -> Option<NodeId> {
    if obj.value.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(obj.value) }.to_string_lossy().into_owned();
    let format = format_from_abi(obj.format);
    let scale = TimeScale::from_i32(obj.scale);
    // A reserved slot has no name and cannot be written.
    format.name()?;

    // A `J`- or `B`-prefixed string stored under `jyear`/`byear` is really
    // the `*_str` "other" form -- astropy accepts only the prefixed strings
    // under those -- so relabel it and let the "other" handling below place
    // it in `base_format`.
    let effective = match (format, value.chars().next()) {
        (TimeFormat::Jyear, Some('J' | 'j')) => TimeFormat::JyearStr,
        (TimeFormat::Byear, Some('B' | 'b')) => TimeFormat::ByearStr,
        _ => format,
    };

    // A numeric "other" format has no string wire form of its own, so its
    // value is rewritten as an ISO date-time from the parsed instant. A value
    // that is already a date-time string -- a `plot_date` just read back from
    // this very form -- is written verbatim.
    let value = if needs_reformat(effective) && infer_format(&value).is_none() {
        let mut parsed = Time::new(value.clone(), format, scale);
        let civil = parsed.compute_civil().ok()?;
        format_isot(&civil)
    } else {
        value
    };

    let mut pairs = Vec::new();
    let mut put = |doc: &mut Document, key: &str, text: String| {
        let k = doc.add_scalar(key);
        let v = doc.add_scalar_styled(text, asdf_core::yaml::ScalarStyle::SingleQuoted);
        pairs.push((k, v));
    };
    put(doc, "value", value);

    // The schema permits only standard formats in `format`; an "other"
    // effective format goes in `base_format` with `format` left out, and its
    // standard wire form is re-inferred from the value on read.
    let name = effective.name()?.to_string();
    put(doc, if effective.is_other() { "base_format" } else { "format" }, name);

    if scale != TimeScale::Utc {
        put(doc, "scale", scale.name().to_string());
    }
    if obj.location.longitude != 0.0 || obj.location.latitude != 0.0 || obj.location.height != 0.0 {
        let mut location = Vec::new();
        for (key, number) in [
            ("longitude", obj.location.longitude),
            ("latitude", obj.location.latitude),
            ("height", obj.location.height),
        ] {
            let k = doc.add_scalar(key);
            let v = doc.add_scalar(asdf_core::core::elements::format_float(number));
            location.push((k, v));
        }
        let node = doc.add_mapping(location);
        let k = doc.add_scalar("location");
        pairs.push((k, node));
    }
    Some(doc.add_mapping(pairs))
}

/// Whether a format's value must be rewritten as a date-time string.
///
/// Only `plot_date` has a numeric scalar form of its own; `ymdhms` and
/// `datetime64` are always stored as ISO strings, and a bare-integer
/// `datetime64` is unit-ambiguous, so neither is reformatted.
fn needs_reformat(format: TimeFormat) -> bool {
    format == TimeFormat::PlotDate
}

/// Render an instant as an `isot` string, trailing zeros trimmed.
fn format_isot(civil: &Civil) -> String {
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        civil.year, civil.month, civil.day, civil.hour, civil.minute, civil.second
    );
    if civil.nanosecond > 0 {
        let fraction = format!("{:09}", civil.nanosecond);
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
    out
}

pub(crate) unsafe fn time_deinit(obj: *mut asdf_time_t) {
    let time = unsafe { &mut *obj };
    if !time.value.is_null() {
        drop(unsafe { CString::from_raw(time.value) });
    }
    *time = asdf_time_t::zeroed();
}

pub(crate) unsafe fn time_copy(src: &asdf_time_t, dst: *mut asdf_time_t) -> bool {
    let out = unsafe { &mut *dst };
    out.value = if src.value.is_null() {
        std::ptr::null_mut()
    } else {
        let text = unsafe { CStr::from_ptr(src.value) };
        match CString::new(text.to_bytes()) {
            Ok(copy) => copy.into_raw(),
            Err(_) => return false,
        }
    };
    out.info = src.info;
    out.format = src.format;
    out.scale = src.scale;
    out.location = src.location;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use asdf_core::yaml::parse_document;

    fn read(yaml: &str) -> asdf_time_t {
        let doc = parse_document(yaml).unwrap();
        let root = doc.root().unwrap();
        let node = doc.mapping_get(root, "t").unwrap();
        let mut time = asdf_time_t::zeroed();
        assert_eq!(time_deserialize(&doc, node, std::ptr::null_mut(), &mut time), AsdfValueErr::Ok);
        time
    }

    fn value_of(time: &asdf_time_t) -> String {
        unsafe { CStr::from_ptr(time.value) }.to_string_lossy().into_owned()
    }

    #[test]
    fn reads_the_bare_string_shorthand() {
        let time = read("t: '2026-09-04T12:00:00'\n");
        assert_eq!(value_of(&time), "2026-09-04T12:00:00");
        assert_eq!(time.format, TimeFormat::Iso as c_int);
        assert_eq!(time.scale, TimeScale::Utc as c_int);
        // The calendar reading is filled in.
        assert_eq!(time.info.tm.tm_year, 2026 - 1900);
        assert_eq!(time.info.tm.tm_mon, 8, "September is month 8 in struct tm");
        assert_eq!(time.info.tm.tm_mday, 4);
        assert_eq!(time.info.tm.tm_hour, 12);
    }

    #[test]
    fn reads_the_mapping_form() {
        let time = read("t:\n  value: 1000000.0\n  format: unix\n  scale: tai\n");
        assert_eq!(value_of(&time), "1000000.0");
        assert_eq!(time.format, TimeFormat::Unix as c_int);
        assert_eq!(time.scale, TimeScale::Tai as c_int);
        assert_eq!(time.info.ts.tv_sec, 1_000_000);
    }

    #[test]
    fn base_format_overrides_format() {
        // The schema's split: `format` carries a standard stand-in and
        // `base_format` the real one, which wins.
        let time = read("t:\n  value: '2026-09-04T12:00:00'\n  format: iso\n  base_format: isot\n");
        assert_eq!(time.format, TimeFormat::Isot as c_int, "base_format must win over format");
    }

    #[test]
    fn reads_a_location() {
        let time = read(
            "t:\n  value: '2026-01-01'\n  format: iso\n  \
             location:\n    longitude: -155.47\n    latitude: 19.82\n    height: 4205.0\n",
        );
        assert!((time.location.longitude - (-155.47)).abs() < 1e-9);
        assert!((time.location.latitude - 19.82).abs() < 1e-9);
        assert!((time.location.height - 4205.0).abs() < 1e-9);
    }

    #[test]
    fn an_unparseable_value_still_round_trips() {
        // The value, format and scale are authoritative; a breakdown that
        // cannot be computed leaves the info zeroed rather than failing.
        let time = read("t:\n  value: 'not a date'\n  format: iso\n");
        assert_eq!(value_of(&time), "not a date");
        assert_eq!(time.info.ts.tv_sec, 0);
    }

    /// Upstream always writes the mapping form, even for a plain UTC ISO
    /// time. Writing the bare-string shorthand instead would be terser but
    /// would not be what a libasdf-written file looks like.
    #[test]
    fn serializes_a_plain_utc_iso_time_as_a_mapping() {
        let mut doc = Document::new_asdf();
        let value = CString::new("2026-09-04T12:00:00").unwrap();
        let time = asdf_time_t { value: value.as_ptr().cast_mut(), ..asdf_time_t::zeroed() };
        let node = time_serialize(&mut doc, &time).unwrap();

        assert!(doc.node(node).is_mapping());
        let written = doc.mapping_get(node, "value").unwrap();
        assert_eq!(doc.node(written).as_str(), Some("2026-09-04T12:00:00"));
        let format = doc.mapping_get(node, "format").unwrap();
        assert_eq!(doc.node(format).as_str(), Some("iso"));
        assert!(doc.mapping_get(node, "scale").is_none(), "UTC is the default");
        assert!(doc.mapping_get(node, "location").is_none(), "no location was set");
    }

    #[test]
    fn serializes_other_formats_into_base_format() {
        let mut doc = Document::new_asdf();
        let value = CString::new("2026-09-04T12:00:00").unwrap();
        let time = asdf_time_t {
            value: value.as_ptr().cast_mut(),
            format: TimeFormat::Isot as c_int,
            ..asdf_time_t::zeroed()
        };
        let node = time_serialize(&mut doc, &time).unwrap();

        // An "other" format is written in `base_format` *instead of*
        // `format`, not alongside it: the schema's `format` enum does not
        // contain it, and a reader re-infers the wire form from the value.
        assert!(doc.node(node).is_mapping());
        assert!(doc.mapping_get(node, "format").is_none(), "`format` is omitted entirely");
        let base = doc.mapping_get(node, "base_format").unwrap();
        assert_eq!(doc.node(base).as_str(), Some("isot"), "the real format goes here");
    }

    /// A `J`- or `B`-prefixed string under `jyear`/`byear` is really the
    /// `*_str` form, and is relabelled so it round-trips as one.
    #[test]
    fn prefixed_epoch_year_strings_become_their_str_forms() {
        for (text, format, expected) in [
            ("J2000.0", TimeFormat::Jyear, "jyear_str"),
            ("B1950.0", TimeFormat::Byear, "byear_str"),
        ] {
            let mut doc = Document::new_asdf();
            let value = CString::new(text).unwrap();
            let time = asdf_time_t {
                value: value.as_ptr().cast_mut(),
                format: format as c_int,
                ..asdf_time_t::zeroed()
            };
            let node = time_serialize(&mut doc, &time).unwrap();
            assert!(doc.mapping_get(node, "format").is_none(), "{text}");
            let base = doc.mapping_get(node, "base_format").unwrap();
            assert_eq!(doc.node(base).as_str(), Some(expected), "{text}");
        }

        // A numeric jyear keeps the standard format.
        let mut doc = Document::new_asdf();
        let value = CString::new("2000.0").unwrap();
        let time = asdf_time_t {
            value: value.as_ptr().cast_mut(),
            format: TimeFormat::Jyear as c_int,
            ..asdf_time_t::zeroed()
        };
        let node = time_serialize(&mut doc, &time).unwrap();
        let format = doc.mapping_get(node, "format").unwrap();
        assert_eq!(doc.node(format).as_str(), Some("jyear"));
    }

    /// `plot_date` has a numeric value with no string wire form, so it is
    /// rewritten as an ISO date-time from the instant it names.
    #[test]
    fn a_numeric_other_format_is_rewritten_as_a_date_time() {
        let mut doc = Document::new_asdf();
        // Matplotlib day 1 is 0001-01-01 in its own reckoning.
        let value = CString::new("739903.5").unwrap();
        let time = asdf_time_t {
            value: value.as_ptr().cast_mut(),
            format: TimeFormat::PlotDate as c_int,
            ..asdf_time_t::zeroed()
        };
        let node = time_serialize(&mut doc, &time).unwrap();

        let written = doc.mapping_get(node, "value").unwrap();
        let text = doc.node(written).as_str().unwrap();
        assert!(text.contains('T'), "a numeric plot_date becomes a date-time: {text}");
        let base = doc.mapping_get(node, "base_format").unwrap();
        assert_eq!(doc.node(base).as_str(), Some("plot_date"));

        // A value that is already a date-time string is written verbatim.
        let mut doc = Document::new_asdf();
        let value = CString::new("2025-10-14T13:26:41").unwrap();
        let time = asdf_time_t {
            value: value.as_ptr().cast_mut(),
            format: TimeFormat::PlotDate as c_int,
            ..asdf_time_t::zeroed()
        };
        let node = time_serialize(&mut doc, &time).unwrap();
        let written = doc.mapping_get(node, "value").unwrap();
        assert_eq!(doc.node(written).as_str(), Some("2025-10-14T13:26:41"));
    }

    #[test]
    fn a_non_utc_scale_is_written_out() {
        let mut doc = Document::new_asdf();
        let value = CString::new("1000.0").unwrap();
        let time = asdf_time_t {
            value: value.as_ptr().cast_mut(),
            format: TimeFormat::Unix as c_int,
            scale: TimeScale::Tai as c_int,
            ..asdf_time_t::zeroed()
        };
        let node = time_serialize(&mut doc, &time).unwrap();
        let scale = doc.mapping_get(node, "scale").unwrap();
        assert_eq!(doc.node(scale).as_str(), Some("tai"));
    }

    #[test]
    fn format_names_match_upstream() {
        let name = |format: c_int| {
            let ptr = asdf_time_format_string(format);
            unsafe { crate::ffi::c_str(ptr) }.map(|s| s.to_string_lossy().into_owned())
        };
        assert_eq!(name(0).as_deref(), Some("iso"));
        assert_eq!(name(10).as_deref(), Some("tai_seconds"));
        assert_eq!(name(22).as_deref(), Some("datetime64"));
        // The reserved slot has no name, as upstream.
        assert_eq!(name(14), None);
        // Out of range is null rather than a crash.
        assert_eq!(name(999), None);
        assert_eq!(name(-1), None);
    }

    #[test]
    fn parse_recomputes_the_breakdown() {
        let value = CString::new("2000-01-01T00:00:00").unwrap();
        let mut time = asdf_time_t { value: value.as_ptr().cast_mut(), ..asdf_time_t::zeroed() };
        assert_eq!(unsafe { asdf_time_parse(&mut time) }, 0);
        assert_eq!(time.info.tm.tm_year, 100);
        assert_eq!(time.info.tm.tm_mon, 0);
        assert_eq!(time.info.tm.tm_mday, 1);

        // A value that will not parse reports failure.
        let bad = CString::new("nonsense").unwrap();
        let mut time = asdf_time_t { value: bad.as_ptr().cast_mut(), ..asdf_time_t::zeroed() };
        assert_eq!(unsafe { asdf_time_parse(&mut time) }, -1);

        assert_eq!(unsafe { asdf_time_parse(std::ptr::null_mut()) }, -1);
    }

    #[test]
    fn struct_tm_conventions_are_honoured() {
        // tm_year counts from 1900, tm_mon from zero, tm_yday from zero.
        let time = read("t: '1970-01-01T00:00:00'\n");
        assert_eq!(time.info.tm.tm_year, 70);
        assert_eq!(time.info.tm.tm_mon, 0);
        assert_eq!(time.info.tm.tm_mday, 1);
        assert_eq!(time.info.tm.tm_yday, 0);
        assert_eq!(time.info.tm.tm_wday, 4, "1970-01-01 was a Thursday");
        assert_eq!(time.info.ts.tv_sec, 0);
    }
}

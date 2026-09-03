//! Resolving a scalar's text into a typed value.
//!
//! YAML leaves scalar typing to the "schema" in force, and the three ASDF
//! implementations do not agree on one:
//!
//! - **libasdf** (via libfyaml) resolves with C's `strtoull`/`strtoll`/`strtod`
//!   at base 0. That gives it C-style octal (`010` is 8) and hex, and means the
//!   YAML float spellings `.inf` and `.nan` resolve as *strings* -- even though
//!   libasdf emits them.
//! - **Python asdf** (via PyYAML) applies the genuine YAML 1.1 resolver, where
//!   `yes`/`no`/`on`/`off` are booleans and sexagesimals are numbers.
//! - **saphyr** implements YAML 1.2.
//!
//! [`Schema::Libasdf`] reproduces the first exactly, so that drop-in parity
//! holds. [`Schema::Yaml11`] implements the second, for reading files written
//! by Python asdf. See `KNOWN-DIVERGENCES.md`.

use crate::node::ScalarStyle;

/// The resolved type of a value, mirroring `asdf_value_type_t`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueType {
    /// Unknown or unresolvable.
    Unknown,
    /// A sequence.
    Sequence,
    /// A mapping.
    Mapping,
    /// A scalar not yet resolved to a narrower type.
    Scalar,
    /// A string.
    String,
    /// A boolean.
    Bool,
    /// A null.
    Null,
    /// Signed 8-bit integer.
    Int8,
    /// Signed 16-bit integer.
    Int16,
    /// Signed 32-bit integer.
    Int32,
    /// Signed 64-bit integer.
    Int64,
    /// Unsigned 8-bit integer.
    Uint8,
    /// Unsigned 16-bit integer.
    Uint16,
    /// Unsigned 32-bit integer.
    Uint32,
    /// Unsigned 64-bit integer.
    Uint64,
    /// 32-bit float.
    Float,
    /// 64-bit float.
    Double,
    /// A registered extension type.
    Extension,
}

impl ValueType {
    /// The name libasdf's `asdf_value_type_string` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            ValueType::Unknown => "<unknown>",
            ValueType::Sequence => "sequence",
            ValueType::Mapping => "mapping",
            ValueType::Scalar => "scalar",
            ValueType::String => "string",
            ValueType::Bool => "bool",
            ValueType::Null => "null",
            ValueType::Int8 => "int8",
            ValueType::Int16 => "int16",
            ValueType::Int32 => "int32",
            ValueType::Int64 => "int64",
            ValueType::Uint8 => "uint8",
            ValueType::Uint16 => "uint16",
            ValueType::Uint32 => "uint32",
            ValueType::Uint64 => "uint64",
            ValueType::Float => "float",
            ValueType::Double => "double",
            ValueType::Extension => "<extension>",
        }
    }

    /// Whether this is any of the integer types.
    pub fn is_int(self) -> bool {
        matches!(
            self,
            ValueType::Int8
                | ValueType::Int16
                | ValueType::Int32
                | ValueType::Int64
                | ValueType::Uint8
                | ValueType::Uint16
                | ValueType::Uint32
                | ValueType::Uint64
        )
    }
}

/// Which scalar-resolution rules to apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Schema {
    /// Reproduce libasdf's C-`strtol`-based resolution exactly. The default,
    /// because drop-in parity is the priority.
    #[default]
    Libasdf,
    /// The genuine YAML 1.1 resolver, as Python asdf applies via PyYAML.
    Yaml11,
}

/// A scalar resolved to a concrete value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Resolved {
    /// A null.
    Null,
    /// A boolean.
    Bool(bool),
    /// A non-negative integer, with the narrowest type that holds it.
    Uint(u64, ValueType),
    /// A negative integer, with the narrowest type that holds it.
    Int(i64, ValueType),
    /// A float. libasdf always reports these as `Double`.
    Double(f64),
    /// Anything else.
    String,
}

impl Resolved {
    /// The `ValueType` this resolution corresponds to.
    pub fn value_type(self) -> ValueType {
        match self {
            Resolved::Null => ValueType::Null,
            Resolved::Bool(_) => ValueType::Bool,
            Resolved::Uint(_, t) | Resolved::Int(_, t) => t,
            Resolved::Double(_) => ValueType::Double,
            Resolved::String => ValueType::String,
        }
    }
}

/// The narrowest unsigned type that holds `v`.
fn narrow_uint(v: u64) -> ValueType {
    if v <= u64::from(u8::MAX) {
        ValueType::Uint8
    } else if v <= u64::from(u16::MAX) {
        ValueType::Uint16
    } else if v <= u64::from(u32::MAX) {
        ValueType::Uint32
    } else {
        ValueType::Uint64
    }
}

/// The narrowest signed type that holds `v`.
fn narrow_int(v: i64) -> ValueType {
    if v >= i64::from(i8::MIN) && v <= i64::from(i8::MAX) {
        ValueType::Int8
    } else if v >= i64::from(i16::MIN) && v <= i64::from(i16::MAX) {
        ValueType::Int16
    } else if v >= i64::from(i32::MIN) && v <= i64::from(i32::MAX) {
        ValueType::Int32
    } else {
        ValueType::Int64
    }
}

/// Is this text one of YAML's null spellings?
pub fn is_null(s: &str) -> bool {
    s.is_empty() || s == "~" || s == "null" || s == "Null" || s == "NULL"
}

/// Parse a boolean the way libasdf does.
///
/// Note `0` and `1` are accepted. Because libasdf tries integers *before*
/// booleans, an untagged `1` resolves as an integer; these two spellings only
/// surface as booleans when the scalar carries an explicit `!!bool` tag.
pub fn parse_bool_libasdf(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// Parse a boolean under YAML 1.1, which admits many more spellings.
pub fn parse_bool_yaml11(s: &str) -> Option<bool> {
    match s {
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Some(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Some(false)
        }
        _ => None,
    }
}

/// Emulate C `strtoull(s, &end, 0)` followed by libasdf's "must consume the
/// whole string" check.
///
/// Base 0 means a `0x`/`0X` prefix selects hex and a bare leading `0` selects
/// octal, which is C's convention rather than YAML's.
fn strtoull_base0_full(s: &str) -> Option<Result<u64, Overflow>> {
    let t = s.trim_start();
    // libasdf explicitly rejects a leading '-' here rather than letting
    // strtoull wrap it around.
    let body = t.strip_prefix('+').unwrap_or(t);
    if !body.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (digits, radix) = split_radix(body);
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    Some(u64::from_str_radix(digits, radix).map_err(|_| Overflow))
}

/// Emulate C `strtoll(s, &end, 0)` with the same whole-string requirement.
fn strtoll_base0_full(s: &str) -> Option<Result<i64, Overflow>> {
    let t = s.trim_start();
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if !body.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (digits, radix) = split_radix(body);
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let signed = if neg { format!("-{digits}") } else { digits.to_string() };
    Some(i64::from_str_radix(&signed, radix).map_err(|_| Overflow))
}

/// Split C's base-0 radix prefix off a digit string.
fn split_radix(body: &str) -> (&str, u32) {
    if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        (hex, 16)
    } else if body.len() > 1 && body.starts_with('0') {
        (&body[1..], 8)
    } else {
        (body, 10)
    }
}

/// Marker for a value that did not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Overflow;

/// Emulate C `strtod` followed by the whole-string check.
///
/// Rust's `f64::from_str` accepts `inf`/`NaN` like C does, but rejects the
/// hex-float form C admits; hex floats do not occur in ASDF trees.
fn strtod_full(s: &str) -> Option<f64> {
    let t = s.trim_start();
    if t.is_empty() {
        return None;
    }
    // Rust accepts a trailing/leading form C would not and vice versa in a few
    // corners; restrict to what both agree on.
    let probe = t.strip_prefix(['+', '-']).unwrap_or(t);
    if !probe.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && !probe.starts_with("inf")
        && !probe.starts_with("Inf")
        && !probe.starts_with("INF")
        && !probe.starts_with("nan")
        && !probe.starts_with("NaN")
        && !probe.starts_with("NAN")
    {
        return None;
    }
    t.parse::<f64>().ok()
}

/// Resolve a plain scalar's text under the given schema.
///
/// `style` is honoured first: a quoted, literal or folded scalar is always a
/// string, whatever its content.
pub fn resolve(text: &str, style: ScalarStyle, schema: Schema) -> Resolved {
    if style.is_quoted() {
        return Resolved::String;
    }
    match schema {
        Schema::Libasdf => resolve_libasdf(text),
        Schema::Yaml11 => resolve_yaml11(text),
    }
}

/// Resolve with an explicit YAML common-schema tag (`!!int`, `!!bool`, ...).
///
/// An explicit tag short-circuits inference, so `!!bool 1` is a boolean where
/// a bare `1` would be an integer.
pub fn resolve_tagged(text: &str, tag_suffix: &str, schema: Schema) -> Option<Resolved> {
    match tag_suffix {
        "null" => Some(if is_null(text) { Resolved::Null } else { Resolved::String }),
        "bool" => {
            let parsed = match schema {
                Schema::Libasdf => parse_bool_libasdf(text),
                Schema::Yaml11 => parse_bool_yaml11(text),
            };
            Some(parsed.map_or(Resolved::String, Resolved::Bool))
        }
        "int" => Some(resolve_int_only(text).unwrap_or(Resolved::String)),
        "float" => Some(strtod_full(text).map_or(Resolved::String, Resolved::Double)),
        "str" => Some(Resolved::String),
        _ => None,
    }
}

fn resolve_int_only(text: &str) -> Option<Resolved> {
    match strtoull_base0_full(text) {
        Some(Ok(v)) => return Some(Resolved::Uint(v, narrow_uint(v))),
        Some(Err(Overflow)) => return Some(Resolved::Uint(u64::MAX, ValueType::Uint64)),
        None => {}
    }
    match strtoll_base0_full(text) {
        Some(Ok(v)) => Some(Resolved::Int(v, narrow_int(v))),
        Some(Err(Overflow)) => Some(Resolved::Int(i64::MIN, ValueType::Int64)),
        None => None,
    }
}

/// libasdf's inference order: null, then int, then bool, then float, else string.
fn resolve_libasdf(text: &str) -> Resolved {
    if is_null(text) {
        return Resolved::Null;
    }
    if let Some(r) = resolve_int_only(text) {
        return r;
    }
    if let Some(b) = parse_bool_libasdf(text) {
        return Resolved::Bool(b);
    }
    if let Some(d) = strtod_full(text) {
        return Resolved::Double(d);
    }
    Resolved::String
}

/// The YAML 1.1 resolver, as PyYAML implements it.
fn resolve_yaml11(text: &str) -> Resolved {
    if is_null(text) {
        return Resolved::Null;
    }
    // 1.1 resolves booleans before numbers, so `y` and `n` never look integral.
    if let Some(b) = parse_bool_yaml11(text) {
        return Resolved::Bool(b);
    }
    if let Some(r) = resolve_int_yaml11(text) {
        return r;
    }
    if let Some(d) = resolve_float_yaml11(text) {
        return Resolved::Double(d);
    }
    Resolved::String
}

fn resolve_int_yaml11(text: &str) -> Option<Resolved> {
    let t = text.replace('_', "");
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, t.strip_prefix('+').unwrap_or(&t).to_string()),
    };
    if body.is_empty() {
        return None;
    }

    let (digits, radix) =
        if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (h.to_string(), 16)
        } else if let Some(o) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (o.to_string(), 8)
        } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (b.to_string(), 2)
        } else if body.contains(':') {
            // Sexagesimal, e.g. 190:20:30
            let mut acc: i128 = 0;
            for part in body.split(':') {
                if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                acc = acc.checked_mul(60)?.checked_add(part.parse::<i128>().ok()?)?;
            }
            let v = if neg { -acc } else { acc };
            return finish_int_yaml11(v);
        } else if body.len() > 1 && body.starts_with('0') {
            (body[1..].to_string(), 8)
        } else {
            (body.clone(), 10)
        };

    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let mag = i128::from_str_radix(&digits, radix).ok()?;
    finish_int_yaml11(if neg { -mag } else { mag })
}

fn finish_int_yaml11(v: i128) -> Option<Resolved> {
    if v >= 0 {
        let u = u64::try_from(v).ok()?;
        Some(Resolved::Uint(u, narrow_uint(u)))
    } else {
        let i = i64::try_from(v).ok()?;
        Some(Resolved::Int(i, narrow_int(i)))
    }
}

fn resolve_float_yaml11(text: &str) -> Option<f64> {
    let t = text.replace('_', "");
    // YAML's own infinity and not-a-number spellings, which libasdf misses.
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => (-1.0, r),
        None => (1.0, t.strip_prefix('+').unwrap_or(&t)),
    };
    match rest {
        ".inf" | ".Inf" | ".INF" => return Some(sign * f64::INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    if rest.contains(':') {
        let mut acc = 0f64;
        for part in rest.split(':') {
            let p: f64 = part.parse().ok()?;
            acc = acc * 60.0 + p;
        }
        return Some(sign * acc);
    }
    strtod_full(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(s: &str) -> Resolved {
        resolve(s, ScalarStyle::Plain, Schema::Libasdf)
    }
    fn y11(s: &str) -> Resolved {
        resolve(s, ScalarStyle::Plain, Schema::Yaml11)
    }

    #[test]
    fn nulls() {
        for s in ["", "~", "null", "Null", "NULL"] {
            assert_eq!(lib(s), Resolved::Null, "{s:?}");
        }
        assert_ne!(lib("nUll"), Resolved::Null);
    }

    #[test]
    fn unsigned_is_preferred_and_narrowed() {
        // libasdf tries unsigned before signed, so a small positive is UINT8.
        assert_eq!(lib("42"), Resolved::Uint(42, ValueType::Uint8));
        assert_eq!(lib("300"), Resolved::Uint(300, ValueType::Uint16));
        assert_eq!(lib("70000"), Resolved::Uint(70000, ValueType::Uint32));
        assert_eq!(lib("5000000000"), Resolved::Uint(5_000_000_000, ValueType::Uint64));
    }

    #[test]
    fn negatives_are_signed_and_narrowed() {
        assert_eq!(lib("-1"), Resolved::Int(-1, ValueType::Int8));
        assert_eq!(lib("-200"), Resolved::Int(-200, ValueType::Int16));
        assert_eq!(lib("-40000"), Resolved::Int(-40000, ValueType::Int32));
    }

    #[test]
    fn base0_radix_matches_c_not_yaml() {
        // C semantics: a bare leading zero is octal, so `010` is 8.
        assert_eq!(lib("010"), Resolved::Uint(8, ValueType::Uint8));
        assert_eq!(lib("0x10"), Resolved::Uint(16, ValueType::Uint8));
        // YAML 1.2's `0o` form is not recognised by libasdf and falls through
        // to a string.
        assert_eq!(lib("0o10"), Resolved::String);
    }

    #[test]
    fn int_is_tried_before_bool() {
        // The consequence of libasdf's ordering: an untagged 1 is an integer,
        // never a boolean, even though its bool parser accepts "1".
        assert_eq!(lib("1"), Resolved::Uint(1, ValueType::Uint8));
        assert_eq!(lib("0"), Resolved::Uint(0, ValueType::Uint8));
        // ...but an explicit tag short-circuits that.
        assert_eq!(resolve_tagged("1", "bool", Schema::Libasdf), Some(Resolved::Bool(true)));
    }

    #[test]
    fn bools() {
        for s in ["true", "True", "TRUE"] {
            assert_eq!(lib(s), Resolved::Bool(true), "{s:?}");
        }
        for s in ["false", "False", "FALSE"] {
            assert_eq!(lib(s), Resolved::Bool(false), "{s:?}");
        }
        // libasdf is case-sensitive about these three spellings only.
        assert_eq!(lib("tRue"), Resolved::String);
    }

    #[test]
    fn floats() {
        assert_eq!(lib("1.5"), Resolved::Double(1.5));
        assert_eq!(lib("1e3"), Resolved::Double(1000.0));
        assert_eq!(lib(".5"), Resolved::Double(0.5));
        assert_eq!(lib("-2.25"), Resolved::Double(-2.25));
    }

    /// Documents a genuine upstream round-trip asymmetry: libasdf *writes*
    /// `.nan` / `.inf` but its `strtod`-based reader rejects them, so they
    /// come back as strings. We reproduce that under `Libasdf` and get it
    /// right under `Yaml11`.
    #[test]
    fn yaml_infinity_spellings_diverge_between_schemas() {
        assert_eq!(lib(".inf"), Resolved::String);
        assert_eq!(lib("-.inf"), Resolved::String);
        assert_eq!(lib(".nan"), Resolved::String);

        assert_eq!(y11(".inf"), Resolved::Double(f64::INFINITY));
        assert_eq!(y11("-.inf"), Resolved::Double(f64::NEG_INFINITY));
        assert!(matches!(y11(".nan"), Resolved::Double(d) if d.is_nan()));
    }

    /// Bare `inf`/`nan` *are* accepted by strtod, so libasdf reads them as
    /// doubles even though it never writes them that way.
    #[test]
    fn bare_inf_and_nan_are_doubles_under_libasdf() {
        assert_eq!(lib("inf"), Resolved::Double(f64::INFINITY));
        assert!(matches!(lib("nan"), Resolved::Double(d) if d.is_nan()));
    }

    #[test]
    fn quoting_forces_string() {
        for style in [
            ScalarStyle::SingleQuoted,
            ScalarStyle::DoubleQuoted,
            ScalarStyle::Literal,
            ScalarStyle::Folded,
        ] {
            assert_eq!(resolve("42", style, Schema::Libasdf), Resolved::String);
            assert_eq!(resolve("true", style, Schema::Libasdf), Resolved::String);
        }
    }

    #[test]
    fn yaml11_bool_spellings() {
        for s in ["yes", "Yes", "YES", "on", "y", "true"] {
            assert_eq!(y11(s), Resolved::Bool(true), "{s:?}");
        }
        for s in ["no", "No", "off", "n", "false"] {
            assert_eq!(y11(s), Resolved::Bool(false), "{s:?}");
        }
        // The divergence that matters for Python-written files.
        assert_eq!(lib("yes"), Resolved::String);
        assert_eq!(y11("yes"), Resolved::Bool(true));
    }

    #[test]
    fn yaml11_underscores_and_sexagesimals() {
        assert_eq!(y11("1_000"), Resolved::Uint(1000, ValueType::Uint16));
        assert_eq!(y11("0o17"), Resolved::Uint(15, ValueType::Uint8));
        // 190:20:30 == 190*3600 + 20*60 + 30
        assert_eq!(y11("190:20:30"), Resolved::Uint(685230, ValueType::Uint32));
    }

    #[test]
    fn overflowing_ints_stay_integers() {
        // libasdf reports overflow but still classes the value as an int.
        let huge = "99999999999999999999999999";
        assert!(matches!(lib(huge), Resolved::Uint(_, ValueType::Uint64)));
    }

    #[test]
    fn plain_text_is_a_string() {
        for s in ["hello", "core/ndarray-1.1.0", "1.2.3", "a b c"] {
            assert_eq!(lib(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn value_type_names_match_libasdf() {
        assert_eq!(ValueType::Uint8.as_str(), "uint8");
        assert_eq!(ValueType::Double.as_str(), "double");
        assert_eq!(ValueType::Unknown.as_str(), "<unknown>");
        assert_eq!(ValueType::Extension.as_str(), "<extension>");
    }
}

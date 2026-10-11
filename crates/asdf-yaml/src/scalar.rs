//! Resolving a scalar's text into a typed value.
//!
//! YAML leaves scalar typing to the "schema" in force, and the three ASDF
//! implementations do not agree on one:
//!
//! - **libasdf** has, since 0.2.1, a YAML 1.1 grammar that resolves ints and
//!   booleans exactly as PyYAML does -- `yes`/`no`/`on`/`off`, `0b`/`0x`/
//!   leading-`0` octal, `_` separators and sexagesimals, but not `y`/`n` or
//!   YAML 1.2's `0o` -- and differs on floats: it also accepts one without a
//!   `.` when it has an exponent (`1e-08`), which earlier versions wrote.
//! - **Python asdf** (via PyYAML) applies the genuine YAML 1.1 resolver, which
//!   requires the `.` in a float and resolves timestamps.
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
    /// Reproduce libasdf's resolution exactly. The default, because drop-in
    /// parity is the priority.
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
    /// An integer literal too large for any of the integer types.
    ///
    /// Distinct from a string: the text *is* a number, so reading it as one
    /// is an overflow rather than a type mismatch, but there is no integer
    /// type that holds it, so its reported type is `Unknown`. libasdf gets
    /// here through `ASDF_VALUE_ERR_OVERFLOW` from its int parser.
    IntOverflow,
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
            Resolved::IntOverflow => ValueType::Unknown,
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
/// These are YAML 1.1's spellings bar `y` and `n`, which neither libasdf nor
/// PyYAML resolve, plus `0` and `1`. Because libasdf tries integers *before*
/// booleans, an untagged `1` resolves as an integer; those two spellings only
/// surface as booleans when the scalar carries an explicit `!!bool` tag.
pub fn parse_bool_libasdf(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => Some(true),
        "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => Some(false),
        _ => None,
    }
}

/// Parse a boolean as PyYAML's YAML 1.1 resolver does.
///
/// The YAML 1.1 type repository also lists `y` / `Y` / `n` / `N`, but PyYAML
/// does not resolve them, and so neither does Python asdf.
pub fn parse_bool_yaml11(s: &str) -> Option<bool> {
    match s {
        "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => Some(true),
        "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => Some(false),
        _ => None,
    }
}

/// Marker for a value that did not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Overflow;

/// Split an optional leading `-` or `+` off, reporting whether it was `-`.
fn split_sign(s: &str) -> (bool, &str) {
    match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    }
}

/// The length of the run of `_` and base-`radix` digits at the start of `s`.
fn span_digits(s: &[u8], radix: u32) -> usize {
    s.iter().take_while(|&&b| b == b'_' || char::from(b).is_digit(radix)).count()
}

/// The length of the match for `(:[0-5]?[0-9])*` at the start of `s`.
///
/// A two-digit component must start `0`-`5`; anything that does ends the
/// match early, and the caller then rejects the scalar for not being consumed.
fn span_base60(s: &[u8]) -> usize {
    let mut i = 0;
    while i + 1 < s.len() && s[i] == b':' && s[i + 1].is_ascii_digit() {
        if i + 2 < s.len() && s[i + 2].is_ascii_digit() {
            if s[i + 1] > b'5' {
                break;
            }
            i += 3;
        } else {
            i += 2;
        }
    }
    i
}

/// Parse `_`-separated digits already known to be valid in `radix`.
///
/// `None` when nothing is left once the separators are gone, so that `0x`
/// and `0b_` are not ints.
fn parse_digits(digits: &str, radix: u32) -> Option<Result<u64, Overflow>> {
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    Some(u64::from_str_radix(&cleaned, radix).map_err(|_| Overflow))
}

/// libasdf's YAML 1.1 integer grammar, as a sign and a magnitude.
///
/// After an optional sign: `0b` binary, `0x` hex, or a leading `0` for octal
/// (which keeps that `0` as a digit, so `0_` is 0); otherwise decimal,
/// optionally followed by base-60 components (`190:20:30`). `_` separates
/// digits anywhere after the first. Unlike PyYAML, YAML 1.2's `0o` and the
/// uppercase `0X` / `0B` are not prefixes.
fn parse_int_libasdf(text: &str) -> Option<Result<(bool, u64), Overflow>> {
    let (negative, body) = split_sign(text);
    let bytes = body.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }

    if bytes[0] == b'0' && bytes.len() > 1 {
        let (digits, radix) = match bytes[1] {
            b'b' => (&body[2..], 2),
            b'x' => (&body[2..], 16),
            _ => (body, 8),
        };
        if span_digits(digits.as_bytes(), radix) != digits.len() {
            return None;
        }
        return parse_digits(digits, radix).map(|r| r.map(|m| (negative, m)));
    }

    let head_len = span_digits(bytes, 10);
    let (head, tail) = body.split_at(head_len);
    if tail.is_empty() {
        return parse_digits(head, 10).map(|r| r.map(|m| (negative, m)));
    }
    if span_base60(tail.as_bytes()) != tail.len() {
        return None;
    }

    let mut magnitude = match parse_digits(head, 10)? {
        Ok(m) => m,
        Err(Overflow) => return Some(Err(Overflow)),
    };
    // `tail` is `:` followed by one- or two-digit components, all validated.
    for component in tail[1..].split(':') {
        let c: u64 = component.parse().ok()?;
        match magnitude.checked_mul(60).and_then(|m| m.checked_add(c)) {
            Some(m) => magnitude = m,
            None => return Some(Err(Overflow)),
        }
    }
    Some(Ok((negative, magnitude)))
}

/// Resolve an int the way libasdf does: non-negative values as the narrowest
/// unsigned type, negative ones as the narrowest signed type.
fn resolve_int_only(text: &str) -> Option<Resolved> {
    match parse_int_libasdf(text)? {
        Ok((false, m)) => Some(Resolved::Uint(m, narrow_uint(m))),
        Ok((true, m)) => Some(match 0i64.checked_sub_unsigned(m) {
            Some(v) => Resolved::Int(v, narrow_int(v)),
            None => Resolved::IntOverflow,
        }),
        // The text is an integer literal that no integer type holds. It is
        // not a string, and it is not a `uint64` either.
        Err(Overflow) => Some(Resolved::IntOverflow),
    }
}

/// The length of the match for `[0-9][0-9_]*` at the start of `s`.
fn span_decimal_int(s: &[u8]) -> usize {
    if s.first().is_some_and(u8::is_ascii_digit) { span_digits(s, 10) } else { 0 }
}

/// The length of the match for `[eE][-+]?[0-9]+` at the start of `s`.
fn span_exponent(s: &[u8]) -> usize {
    if !matches!(s.first(), Some(b'e' | b'E')) {
        return 0;
    }
    let start = if matches!(s.get(1), Some(b'-' | b'+')) { 2 } else { 1 };
    let digits = s[start.min(s.len())..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 { 0 } else { start + digits }
}

/// libasdf's decimal float grammar.
///
/// A mantissa of `[0-9][0-9_]*` with an optional `.[0-9_]*`, or
/// `.[0-9][0-9_]*`, then an optional `[eE][-+]?[0-9]+`. Either the `.` or the
/// exponent is required unless `allow_int`, which `!!float` sets. That is a
/// little more lenient than YAML 1.1, which requires the `.` and a signed
/// exponent: files written before libasdf 0.2.1 contain `1e-08`.
fn is_decimal_float(text: &str, allow_int: bool) -> bool {
    let (_, body) = split_sign(text);
    let s = body.as_bytes();
    let int_len = span_decimal_int(s);
    let mut i = int_len;
    let mut has_point = false;

    if s.get(i) == Some(&b'.') {
        has_point = true;
        i += 1;
        // `.5` is a float; `.` and `._` are not.
        if int_len == 0 && span_decimal_int(&s[i..]) == 0 {
            return false;
        }
        i += span_digits(&s[i..], 10);
    } else if int_len == 0 {
        return false;
    }

    let exp_len = span_exponent(&s[i..]);
    i += exp_len;
    i == s.len() && (has_point || exp_len > 0 || allow_int)
}

/// libasdf's base-60 float grammar: `[-+]?[0-9][0-9_]*(:[0-5]?[0-9])+\.[0-9_]*`.
fn is_base60_float(text: &str) -> bool {
    let (_, body) = split_sign(text);
    let s = body.as_bytes();
    let int_len = span_decimal_int(s);
    if int_len == 0 {
        return false;
    }
    let base60_len = span_base60(&s[int_len..]);
    if base60_len == 0 {
        return false;
    }
    let i = int_len + base60_len;
    if s.get(i) != Some(&b'.') {
        return false;
    }
    i + 1 + span_digits(&s[i + 1..], 10) == s.len()
}

/// Parse float text already validated by one of the grammars above.
///
/// A magnitude too large for a double comes out infinite, as `strtod` does;
/// underflow gives the nearest subnormal or zero.
fn parse_float_digits(s: &str) -> Option<f64> {
    s.chars().filter(|&c| c != '_').collect::<String>().parse().ok()
}

/// Parse a base-60 float. As in PyYAML, each component is parsed as a float,
/// so the last one carries the fraction.
fn parse_base60_float(text: &str) -> Option<f64> {
    let (negative, body) = split_sign(text);
    let mut value = 0.0;
    for component in body.split(':') {
        value = value * 60.0 + parse_float_digits(component)?;
    }
    Some(if negative { -value } else { value })
}

/// YAML's non-finite float spellings, in the three cases the standard allows.
///
/// Infinity may carry a sign; NaN may not. PyYAML's resolver and libasdf's
/// `is_yaml_special_float` agree on both halves, so `-.nan` is a string.
fn special_float(text: &str) -> Option<f64> {
    if let ".nan" | ".NaN" | ".NAN" = text {
        return Some(f64::NAN);
    }
    let (negative, body) = split_sign(text);
    match body {
        ".inf" | ".Inf" | ".INF" => Some(if negative { f64::NEG_INFINITY } else { f64::INFINITY }),
        _ => None,
    }
}

/// libasdf's float resolution: decimal, then the non-finite spellings, then
/// base 60.
fn parse_float_libasdf(text: &str, allow_int: bool) -> Option<f64> {
    if is_decimal_float(text, allow_int) {
        parse_float_digits(text)
    } else if let Some(d) = special_float(text) {
        Some(d)
    } else if is_base60_float(text) {
        parse_base60_float(text)
    } else {
        None
    }
}

/// Would a plain scalar with this text resolve as an int or a float?
///
/// libasdf writes a numeric time `value` verbatim on this basis, so that it
/// keeps its full precision.
pub fn is_number(text: &str) -> bool {
    !text.is_empty()
        && (parse_int_libasdf(text).is_some() || parse_float_libasdf(text, false).is_some())
}

/// Would this string read back as something other than a string if it were
/// written plain, and so need quoting?
///
/// This is libasdf's `asdf_yaml_string_is_ambiguous`, which errs on the side
/// of quoting: besides its own resolution rules it covers what a YAML 1.2
/// core-schema reader or PyYAML would take as a non-string -- `0o17`, `y` and
/// `n`, timestamps, and the `<<` merge key.
pub fn is_ambiguous_string(s: &str) -> bool {
    if is_null(s) || (s.len() > 1 && parse_bool_libasdf(s).is_some()) {
        return true;
    }
    if matches!(s, "<<" | "=" | "y" | "Y" | "n" | "N") {
        return true;
    }
    // Every int, float and timestamp starts with a sign, a digit or a `.`.
    if !s.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.')) {
        return false;
    }
    if parse_int_libasdf(s).is_some() || parse_float_libasdf(s, true).is_some() {
        return true;
    }

    // YAML 1.2's octal, `0o[0-7]+`.
    let (_, body) = split_sign(s);
    if let Some(octal) = body.strip_prefix("0o")
        && !octal.is_empty()
        && span_digits(octal.as_bytes(), 8) == octal.len()
    {
        return true;
    }

    // Every YAML 1.1 timestamp starts with a four-digit year and a `-`.
    let b = s.as_bytes();
    b.len() > 4 && span_digits(&b[..4], 10) == 4 && b[4] == b'-'
}

/// The style a string has to be written in to read back as a string.
pub fn string_style(s: &str) -> ScalarStyle {
    if is_ambiguous_string(s) { ScalarStyle::SingleQuoted } else { ScalarStyle::Plain }
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
/// a bare `1` would be an integer, and `!!float 5` is a float.
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
        "float" => Some(parse_float_libasdf(text, true).map_or(Resolved::String, Resolved::Double)),
        "str" => Some(Resolved::String),
        _ => None,
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
    if let Some(d) = parse_float_libasdf(text, false) {
        return Resolved::Double(d);
    }
    Resolved::String
}

/// PyYAML's resolver, the genuine YAML 1.1 one: null, bool, float, int, else
/// string.
///
/// The grammars below are PyYAML's implicit-resolver regular expressions, one
/// function per alternative, and the values are what its `SafeConstructor`
/// builds from them. Its timestamps have no `Resolved` form and stay strings.
fn resolve_yaml11(text: &str) -> Resolved {
    if is_null(text) {
        return Resolved::Null;
    }
    if let Some(b) = parse_bool_yaml11(text) {
        return Resolved::Bool(b);
    }
    if let Some(d) = parse_float_yaml11(text) {
        return Resolved::Double(d);
    }
    resolve_int_yaml11(text).unwrap_or(Resolved::String)
}

/// Does `s` start with `[0-9][0-9_]*`, and how long is that run?
fn span_leading_digits(s: &[u8], first: impl Fn(u8) -> bool) -> usize {
    if s.first().is_some_and(|&b| first(b)) { span_digits(s, 10) } else { 0 }
}

/// PyYAML's int, as a sign and a magnitude:
///
/// ```text
/// [-+]?0b[0-1_]+ | [-+]?0[0-7_]+ | [-+]?(?:0|[1-9][0-9_]*)
///   | [-+]?0x[0-9a-fA-F_]+ | [-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+
/// ```
///
/// `0b_` and `0x_` match but have no digits, so PyYAML's constructor raises
/// on them rather than returning anything; they are strings here.
fn parse_int_yaml11(text: &str) -> Option<Result<(bool, u64), Overflow>> {
    let (negative, body) = split_sign(text);
    let b = body.as_bytes();
    let with_sign = |r: Option<Result<u64, Overflow>>| r.map(|r| r.map(|m| (negative, m)));

    if let Some(digits) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0x")) {
        let radix = if b[1] == b'b' { 2 } else { 16 };
        if digits.is_empty() || span_digits(digits.as_bytes(), radix) != digits.len() {
            return None;
        }
        return with_sign(parse_digits(digits, radix));
    }
    if b.len() > 1 && b[0] == b'0' {
        return if span_digits(b, 8) == b.len() { with_sign(parse_digits(body, 8)) } else { None };
    }
    if body == "0" {
        return Some(Ok((negative, 0)));
    }

    let head = span_leading_digits(b, |c| (b'1'..=b'9').contains(&c));
    if head == 0 {
        return None;
    }
    if head == b.len() {
        return with_sign(parse_digits(body, 10));
    }
    let tail = &body[head..];
    if span_base60(tail.as_bytes()) != tail.len() {
        return None;
    }
    let mut magnitude = match parse_digits(&body[..head], 10)? {
        Ok(m) => m,
        Err(Overflow) => return Some(Err(Overflow)),
    };
    for component in tail[1..].split(':') {
        let c: u64 = component.parse().ok()?;
        match magnitude.checked_mul(60).and_then(|m| m.checked_add(c)) {
            Some(m) => magnitude = m,
            None => return Some(Err(Overflow)),
        }
    }
    Some(Ok((negative, magnitude)))
}

/// Resolve a PyYAML int. Python's ints have no ceiling, so one too large for
/// `u64` or `i64` is [`Resolved::IntOverflow`] rather than a string.
fn resolve_int_yaml11(text: &str) -> Option<Resolved> {
    match parse_int_yaml11(text)? {
        Ok((false, m)) => Some(Resolved::Uint(m, narrow_uint(m))),
        Ok((true, m)) => Some(match 0i64.checked_sub_unsigned(m) {
            // Python's `-0` is plain `0`, which is non-negative.
            Some(0) => Resolved::Uint(0, ValueType::Uint8),
            Some(v) => Resolved::Int(v, narrow_int(v)),
            None => Resolved::IntOverflow,
        }),
        Err(Overflow) => Some(Resolved::IntOverflow),
    }
}

/// The length of `(?:[eE][-+][0-9]+)?` at the start of `s`: unlike libasdf,
/// PyYAML requires the exponent's sign.
fn span_signed_exponent(s: &[u8]) -> usize {
    if !matches!(s.first(), Some(b'e' | b'E')) || !matches!(s.get(1), Some(b'-' | b'+')) {
        return 0;
    }
    let digits = s[2..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 { 0 } else { 2 + digits }
}

/// PyYAML's float:
///
/// ```text
/// [-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?
///   | \.[0-9][0-9_]*(?:[eE][-+][0-9]+)?
///   | [-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*
///   | [-+]?\.(?:inf|Inf|INF) | \.(?:nan|NaN|NAN)
/// ```
///
/// So the `.` is required, an exponent must be signed, and a float that
/// starts with its `.` cannot be: `1.5e3` and `-.5` are strings.
fn parse_float_yaml11(text: &str) -> Option<f64> {
    if let Some(d) = special_float(text) {
        return Some(d);
    }
    if let Some(rest) = text.strip_prefix('.') {
        let r = rest.as_bytes();
        let frac = span_leading_digits(r, |c| c.is_ascii_digit());
        return (frac > 0 && frac + span_signed_exponent(&r[frac..]) == r.len())
            .then(|| parse_float_digits(text))?;
    }

    let (negative, body) = split_sign(text);
    let b = body.as_bytes();
    let head = span_leading_digits(b, |c| c.is_ascii_digit());
    if head == 0 {
        return None;
    }
    let base60 = span_base60(&b[head..]);
    let point = head + base60;
    if b.get(point) != Some(&b'.') {
        return None;
    }
    let frac_end = point + 1 + span_digits(&b[point + 1..], 10);

    if base60 > 0 {
        // As PyYAML builds it: each component a float, the last carrying
        // the fraction.
        return (frac_end == b.len()).then(|| parse_base60_float(text))?;
    }
    (frac_end + span_signed_exponent(&b[frac_end..]) == b.len())
        .then(|| parse_float_digits(body).map(|v| if negative { -v } else { v }))?
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
    fn radix_prefixes_follow_yaml11() {
        // A bare leading zero is octal, so `010` is 8, and `0_` is still 0.
        assert_eq!(lib("010"), Resolved::Uint(8, ValueType::Uint8));
        assert_eq!(lib("0_"), Resolved::Uint(0, ValueType::Uint8));
        assert_eq!(lib("0x10"), Resolved::Uint(16, ValueType::Uint8));
        assert_eq!(lib("0xFF"), Resolved::Uint(255, ValueType::Uint8));
        assert_eq!(lib("0b101"), Resolved::Uint(5, ValueType::Uint8));
        assert_eq!(lib("-0x10"), Resolved::Int(-16, ValueType::Int8));
        // YAML 1.2's `0o` and the uppercase prefixes are not prefixes to
        // libasdf, and neither is a prefix with no digits after it.
        for s in ["0o10", "0X10", "0B1", "0x", "0b_", "09"] {
            assert_eq!(lib(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn underscores_and_sexagesimals() {
        assert_eq!(lib("1_000"), Resolved::Uint(1000, ValueType::Uint16));
        assert_eq!(lib("0x_ff"), Resolved::Uint(255, ValueType::Uint8));
        // 190:20:30 == 190*3600 + 20*60 + 30
        assert_eq!(lib("190:20:30"), Resolved::Uint(685_230, ValueType::Uint32));
        assert_eq!(lib("-1:30"), Resolved::Int(-90, ValueType::Int8));
        assert_eq!(lib("1:7"), Resolved::Uint(67, ValueType::Uint8));
        assert_eq!(lib("1:30.5"), Resolved::Double(90.5));
        assert_eq!(lib("1_000.5"), Resolved::Double(1000.5));
        // A two-digit component past 59, or a third digit, is not base 60.
        for s in ["1:60", "1:123", "1:", ":30", "1::2"] {
            assert_eq!(lib(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn base60_overflow_is_an_overflow() {
        assert_eq!(lib("18446744073709551615:0"), Resolved::IntOverflow);
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
        for s in ["yes", "Yes", "YES", "on", "On", "ON"] {
            assert_eq!(lib(s), Resolved::Bool(true), "{s:?}");
        }
        for s in ["no", "No", "NO", "off", "Off", "OFF"] {
            assert_eq!(lib(s), Resolved::Bool(false), "{s:?}");
        }
        // Case matters, and YAML 1.1's `y` and `n` are not booleans to
        // libasdf (or to PyYAML).
        for s in ["tRue", "yES", "y", "n", "Y", "N"] {
            assert_eq!(lib(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn floats() {
        assert_eq!(lib("1.5"), Resolved::Double(1.5));
        assert_eq!(lib("1e3"), Resolved::Double(1000.0));
        assert_eq!(lib(".5"), Resolved::Double(0.5));
        assert_eq!(lib("-2.25"), Resolved::Double(-2.25));
        assert_eq!(lib("1."), Resolved::Double(1.0));
        assert_eq!(lib("1.0e-08"), Resolved::Double(1.0e-8));
        // Accepted for files written before libasdf 0.2.1, which left the
        // `.` out: YAML 1.1 proper would make these strings.
        assert_eq!(lib("1e-08"), Resolved::Double(1.0e-8));
        assert_eq!(lib("1E+5"), Resolved::Double(1.0e5));
        // A `.` or an exponent is required, and an exponent needs digits.
        for s in [".", "._", "1e", "1e+", "e5", "1.5.5", "0x1p3", "1.5f"] {
            assert_eq!(lib(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn a_float_tag_accepts_an_int() {
        assert_eq!(resolve_tagged("5", "float", Schema::Libasdf), Some(Resolved::Double(5.0)));
        assert_eq!(resolve_tagged("1_0", "float", Schema::Libasdf), Some(Resolved::Double(10.0)));
        // ...but only a decimal one.
        assert_eq!(resolve_tagged("0x10", "float", Schema::Libasdf), Some(Resolved::String));
    }

    #[test]
    fn ambiguous_strings_are_the_ones_any_reader_would_retype() {
        for s in [
            "",
            "~",
            "null",
            "true",
            "yes",
            "Off",
            "y",
            "N",
            "<<",
            "=",
            "0",
            "123",
            "-1",
            "1_000",
            "0x1f",
            "1:30",
            "1.5",
            ".5",
            "1e5",
            ".inf",
            "-.inf",
            ".nan",
            "0o17",
            "+0o7",
            "2001-12-14",
            "2001-12-14t21:59:43.10-05:00",
        ] {
            assert!(is_ambiguous_string(s), "{s:?} should be quoted");
            assert_eq!(string_style(s), ScalarStyle::SingleQuoted, "{s:?}");
        }
        for s in [
            "hello", "inf", "nan", "-", "+", ".", "1.2.3", "0o", "0o8", "200-1", "yess", "core",
            "1a",
        ] {
            assert!(!is_ambiguous_string(s), "{s:?} should stay plain");
            assert_eq!(string_style(s), ScalarStyle::Plain, "{s:?}");
        }
    }

    #[test]
    fn numbers_are_ints_and_floats_but_not_the_rest() {
        for s in ["2451545.0", "59000", "1e5", "-1:30", ".inf", "99999999999999999999999"] {
            assert!(is_number(s), "{s:?}");
        }
        for s in ["", "null", "true", "2000-01-01", "J2000.0", "inf"] {
            assert!(!is_number(s), "{s:?}");
        }
    }

    /// The non-finite spellings both schemas agree on, in all three cases.
    /// libasdf emits these and, since 0.2.0, reads them back -- closing a
    /// round-trip asymmetry this crate used to have to reproduce.
    #[test]
    fn yaml_infinity_spellings_resolve_under_both_schemas() {
        for resolve in [lib as fn(&str) -> Resolved, y11] {
            for s in [".inf", ".Inf", ".INF", "+.inf"] {
                assert_eq!(resolve(s), Resolved::Double(f64::INFINITY), "{s:?}");
            }
            for s in ["-.inf", "-.Inf", "-.INF"] {
                assert_eq!(resolve(s), Resolved::Double(f64::NEG_INFINITY), "{s:?}");
            }
            for s in [".nan", ".NaN", ".NAN"] {
                assert!(matches!(resolve(s), Resolved::Double(d) if d.is_nan()), "{s:?}");
            }
        }
    }

    /// The spellings that look non-finite and are not. `strtod` accepts the
    /// bare forms, and accepting them would make `inf` in a tree a float
    /// where every other YAML reader sees a string; a mixed-case `.iNf` and
    /// a signed NaN are in neither grammar.
    #[test]
    fn near_miss_non_finite_spellings_are_strings() {
        for s in ["inf", "Inf", "-inf", "infinity", "nan", "NaN", ".iNf", "-.nan", "+.nan"] {
            assert_eq!(lib(s), Resolved::String, "{s:?}");
            assert_eq!(y11(s), Resolved::String, "{s:?}");
        }
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
        for s in ["yes", "Yes", "YES", "on", "true"] {
            assert_eq!(y11(s), Resolved::Bool(true), "{s:?}");
        }
        for s in ["no", "No", "off", "false"] {
            assert_eq!(y11(s), Resolved::Bool(false), "{s:?}");
        }
        // The YAML 1.1 type repository lists `y` and `n`, but PyYAML -- and
        // so Python asdf -- reads them as strings.
        for s in ["y", "Y", "n", "N"] {
            assert_eq!(y11(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn yaml11_underscores_and_sexagesimals() {
        assert_eq!(y11("1_000"), Resolved::Uint(1000, ValueType::Uint16));
        // 190:20:30 == 190*3600 + 20*60 + 30
        assert_eq!(y11("190:20:30"), Resolved::Uint(685_230, ValueType::Uint32));
        // PyYAML has no `0o` or uppercase prefix, needs a digit before any
        // `_`, and bounds a two-digit base-60 component at 59.
        for s in ["0o17", "0X1F", "0B1", "_1", "08", "1:60", "0:30"] {
            assert_eq!(y11(s), Resolved::String, "{s:?}");
        }
    }

    #[test]
    fn yaml11_floats_need_a_point_and_a_signed_exponent() {
        assert_eq!(y11("1.5e+3"), Resolved::Double(1500.0));
        assert_eq!(y11(".5"), Resolved::Double(0.5));
        assert_eq!(y11("1._"), Resolved::Double(1.0));
        assert_eq!(y11("-1:30.5"), Resolved::Double(-90.5));
        // libasdf reads all of these as floats; PyYAML does not.
        for s in ["1e5", "1.5e3", "1e-08", "-.5", "+.5"] {
            assert_eq!(y11(s), Resolved::String, "{s:?}");
            assert!(matches!(lib(s), Resolved::Double(_)), "{s:?}");
        }
    }

    /// An integer literal too large for any integer type is neither an
    /// integer nor a string.
    ///
    /// libasdf reaches this through its int parser overflowing: the value's
    /// reported type stays `<unknown>`, every `asdf_value_is_*` is false,
    /// and every `asdf_value_as_*` reports an overflow rather than a type
    /// mismatch -- because the text *is* a number, just not one that fits.
    #[test]
    fn an_integer_too_large_for_any_type_is_neither_int_nor_string() {
        for huge in [
            "99999999999999999999999999",
            // Exactly one past `u64::MAX`.
            "18446744073709551616",
            "-99999999999999999999999999",
        ] {
            assert_eq!(lib(huge), Resolved::IntOverflow, "{huge}");
            assert_eq!(lib(huge).value_type(), ValueType::Unknown, "{huge}");
        }

        // The largest values that *do* fit are unaffected.
        assert_eq!(lib("18446744073709551615"), Resolved::Uint(u64::MAX, ValueType::Uint64));
        assert_eq!(lib("-9223372036854775808"), Resolved::Int(i64::MIN, ValueType::Int64));
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

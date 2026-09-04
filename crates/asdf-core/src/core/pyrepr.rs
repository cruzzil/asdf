//! Python's `repr` spelling for floats and complex numbers.
//!
//! The ASDF Standard's `core/complex` schema does not pin a spelling, and in
//! practice the corpus is written by Python `asdf`, so the canonical text for
//! a complex value is whatever CPython's `complex.__repr__` produces --
//! `0j`, `(-0+0j)`, `(nan-infj)`, `1.7976931348623157e+308j`. Reproducing it
//! is what lets a re-emitted tree compare equal to the reference `.yaml`
//! files character for character.
//!
//! CPython builds these with `PyOS_double_to_string(x, 'r', 0, flags, NULL)`:
//! the shortest decimal that round-trips, switched to exponent notation when
//! the decimal point would fall outside a fixed window. Rust's `{:e}` gives
//! the same shortest digits, so the work here is placing the point and the
//! exponent the way CPython does.

/// The decimal point positions CPython's `'r'` format keeps in fixed
/// notation: `decpt <= -4 || decpt > 16` switches to an exponent.
const FIXED_MIN_DECPT: i32 = -4;
const FIXED_MAX_DECPT: i32 = 16;

/// Format a float as CPython's `repr` does, optionally forcing a `+` sign.
///
/// This is `PyOS_double_to_string(value, 'r', 0, ..)` *without*
/// `Py_DTSF_ADD_DOT_0`, which is the form `complex.__repr__` uses -- so a
/// whole number comes out as `1`, not `1.0`. [`repr_f64`] adds the `.0`.
fn double_to_string(value: f64, force_sign: bool) -> String {
    let plus = if force_sign && !value.is_sign_negative() { "+" } else { "" };

    if value.is_nan() {
        // A NaN's sign bit is not reported; CPython prints it unsigned.
        return format!("{}nan", if force_sign { "+" } else { "" });
    }
    if value.is_infinite() {
        return format!("{plus}{}", if value.is_sign_negative() { "-inf" } else { "inf" });
    }

    // `{:e}` gives the shortest round-tripping digits with an explicit
    // exponent, which is exactly what CPython's shortest-repr produces.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific.as_str(), "0"));
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);

    // `decpt` is the number of digits that belong before the decimal point.
    let decpt = exponent + 1;
    let sign = if negative { "-" } else { plus };

    if decpt <= FIXED_MIN_DECPT || decpt > FIXED_MAX_DECPT {
        let mut out = String::from(sign);
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        // The exponent always carries a sign and at least two digits.
        let exp = decpt - 1;
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
        return out;
    }

    let mut out = String::from(sign);
    if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..-decpt {
            out.push('0');
        }
        out.push_str(&digits);
    } else if decpt as usize >= digits.len() {
        out.push_str(&digits);
        for _ in 0..(decpt as usize - digits.len()) {
            out.push('0');
        }
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    out
}

/// Format a float as Python's `repr` does: `1.0`, `-0.0`, `1e+16`, `nan`.
pub fn repr_f64(value: f64) -> String {
    let s = double_to_string(value, false);
    // `Py_DTSF_ADD_DOT_0`: a fixed-notation result with no point gets one, so
    // that it reads back as a float rather than an integer.
    if s.contains('.') || s.contains('e') || s.contains("nan") || s.contains("inf") {
        s
    } else {
        format!("{s}.0")
    }
}

/// Format a complex number as Python's `repr` does.
///
/// A value whose real part is `+0.0` is written as the imaginary part alone
/// (`3j`); anything else is parenthesised with the imaginary part's sign made
/// explicit (`(1+2j)`, `(-0+0j)`, `(nan-infj)`). Note the real part is
/// written *without* a trailing `.0`, which is why `(-0+0j)` is not
/// `(-0.0+0.0j)`.
pub fn repr_complex(real: f64, imaginary: f64) -> String {
    if real == 0.0 && real.is_sign_positive() {
        return format!("{}j", double_to_string(imaginary, false));
    }
    format!("({}{}j)", double_to_string(real, false), double_to_string(imaginary, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every expectation here is `repr(x)` from CPython 3.
    #[test]
    fn floats_match_python_repr() {
        assert_eq!(repr_f64(0.0), "0.0");
        assert_eq!(repr_f64(-0.0), "-0.0");
        assert_eq!(repr_f64(1.0), "1.0");
        assert_eq!(repr_f64(-1.5), "-1.5");
        assert_eq!(repr_f64(0.1), "0.1");
        assert_eq!(repr_f64(1e15), "1000000000000000.0");
        // The fixed/exponent switch: decpt > 16 goes exponential.
        assert_eq!(repr_f64(1e16), "1e+16");
        assert_eq!(repr_f64(1e-4), "0.0001");
        assert_eq!(repr_f64(1e-5), "1e-05");
        assert_eq!(repr_f64(f64::MAX), "1.7976931348623157e+308");
        assert_eq!(repr_f64(f64::MIN_POSITIVE), "2.2250738585072014e-308");
        assert_eq!(repr_f64(f64::EPSILON), "2.220446049250313e-16");
        assert_eq!(repr_f64(f64::INFINITY), "inf");
        assert_eq!(repr_f64(f64::NEG_INFINITY), "-inf");
        assert_eq!(repr_f64(f64::NAN), "nan");
    }

    /// Every expectation here is `repr(complex(re, im))` from CPython 3, and
    /// several are taken verbatim from `reference_files/1.6.0/complex.yaml`.
    #[test]
    fn complex_matches_python_repr() {
        assert_eq!(repr_complex(0.0, 0.0), "0j");
        assert_eq!(repr_complex(-0.0, 0.0), "(-0+0j)");
        assert_eq!(repr_complex(1.0, 2.0), "(1+2j)");
        assert_eq!(repr_complex(1.0, -2.0), "(1-2j)");
        assert_eq!(repr_complex(0.0, -1.0), "-1j");
        assert_eq!(repr_complex(f64::NAN, f64::NAN), "(nan+nanj)");
        assert_eq!(repr_complex(f64::NAN, f64::INFINITY), "(nan+infj)");
        assert_eq!(repr_complex(f64::NAN, f64::NEG_INFINITY), "(nan-infj)");
        assert_eq!(repr_complex(0.0, -f64::MAX), "-1.7976931348623157e+308j");
        assert_eq!(repr_complex(0.0, f64::EPSILON), "2.220446049250313e-16j");
    }

    /// The point of the exercise: every value round-trips through the text.
    #[test]
    fn the_shortest_form_still_round_trips() {
        let values = [
            0.1,
            1.0 / 3.0,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            1e16,
            1e-5,
            -2.5e-300,
            123456789012345678.0,
        ];
        for v in values {
            let text = repr_f64(v);
            let back: f64 = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(back.to_bits(), v.to_bits(), "{v} -> {text}");
        }
    }
}

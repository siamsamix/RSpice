/// Parse SPICE-style numeric literals with optional SI suffixes.
///
/// The scale suffix is the *first* alphabetic run after the number, not the
/// last character: `4.7Meg` is 4.7·1e6, `1kOhm` is 1e3, and a bare unit letter
/// (`5V`, `2Hz`) passes through unscaled — letters that carry no scale meaning
/// are ignored, matching both reference tools. `MEG` (1e6) and `MIL`
/// (25.4e-6 m) are the recognized multi-letter prefixes and must win before
/// the single-letter rule turns `Meg` into giga.
pub fn parse_value(token: &str) -> Result<f64, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty value".into());
    }

    let bytes = token.as_bytes();
    let mut i = 0usize;

    // Optional leading sign.
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }

    // Digits and decimal points.
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }

    // Exponent: `e`/`E` only counts when followed by (sign and) digits —
    // otherwise the `e` belongs to the suffix/unit field (`1er` is not 1 exa-…).
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }

    if i == 0 {
        return Err(format!("invalid number '{token}'"));
    }
    let (num_part, suffix) = token.split_at(i);
    let value: f64 = num_part
        .parse()
        .map_err(|_| format!("invalid number '{token}'"))?;
    Ok(value * suffix_multiplier(suffix)?)
}

/// Scale multiplier for whatever trails the numeric part.
///
/// The whole alphabetic tail (`k`, `MEG`, `kOhm`, `5V`'s `V`) arrives here;
/// the first letters decide the multiplier and the rest is decoration.
fn suffix_multiplier(suffix: &str) -> Result<f64, String> {
    let suffix = suffix.trim();
    if suffix.is_empty() {
        return Ok(1.0);
    }
    let lower = suffix.to_ascii_lowercase();
    // Multi-letter scale words first — `4.7Meg` must not see the `g`.
    if lower.starts_with("meg") {
        return Ok(1e6);
    }
    if lower.starts_with("mil") {
        return Ok(25.4e-6); // 1/1000 inch
    }
    match lower.as_bytes()[0] {
        b't' => Ok(1e12),
        b'g' => Ok(1e9),
        b'k' => Ok(1e3),
        b'm' => Ok(1e-3),
        b'u' => Ok(1e-6),
        b'n' => Ok(1e-9),
        b'p' => Ok(1e-12),
        b'f' => Ok(1e-15),
        // Not a scale letter: SPICE ignores trailing unit text (`5V`, `2Hz`,
        // `10ohm`), so these stay unscaled instead of erroring.
        _ => Ok(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_value;

    #[test]
    fn parses_suffixes() {
        assert!((parse_value("1k").unwrap() - 1000.0).abs() < 1e-9);
        assert!((parse_value("1u").unwrap() - 1e-6).abs() < 1e-15);
        assert!((parse_value("4.7").unwrap() - 4.7).abs() < 1e-9);
    }

    #[test]
    fn meg_wins_over_single_letter_rule() {
        // The old parser took the LAST character, so `4.7Meg` mis-parsed as giga.
        assert!((parse_value("4.7Meg").unwrap() - 4.7e6).abs() < 1e-3);
        assert!((parse_value("1meg").unwrap() - 1e6).abs() < 1e-3);
        assert!((parse_value("2MEGOHM").unwrap() - 2e6).abs() < 1e-3);
    }

    #[test]
    fn mil_is_a_thousandth_of_an_inch() {
        assert!((parse_value("3mil").unwrap() - 3.0 * 25.4e-6).abs() < 1e-18);
    }

    #[test]
    fn trailing_unit_letters_are_ignored() {
        assert!((parse_value("5V").unwrap() - 5.0).abs() < 1e-9);
        assert!((parse_value("2Hz").unwrap() - 2.0).abs() < 1e-9);
        assert!((parse_value("10kOhm").unwrap() - 1e4).abs() < 1e-9);
        assert!((parse_value("1uF").unwrap() - 1e-6).abs() < 1e-18);
    }

    #[test]
    fn exponents_still_work() {
        assert!((parse_value("1e3").unwrap() - 1e3).abs() < 1e-9);
        assert!((parse_value("1E-6").unwrap() - 1e-6).abs() < 1e-30);
        assert!((parse_value("-2.5e+2").unwrap() + 250.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_junk_but_accepts_signed_leading_plus() {
        assert!(parse_value("").is_err());
        assert!(parse_value("k").is_err());
        assert!(parse_value("abc").is_err());
        assert!((parse_value("+5").unwrap() - 5.0).abs() < 1e-9);
    }
}

/// Parse SPICE-style numeric literals with optional SI suffixes.
pub fn parse_value(token: &str) -> Result<f64, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty value".into());
    }

    let (num_part, multiplier) = if let Some(last) = token.chars().last() {
        if last.is_ascii_alphabetic() {
            let mult = suffix_multiplier(last)?;
            (&token[..token.len() - 1], mult)
        } else {
            (token, 1.0)
        }
    } else {
        (token, 1.0)
    };

    let value: f64 = num_part
        .parse()
        .map_err(|_| format!("invalid number '{token}'"))?;
    Ok(value * multiplier)
}

fn suffix_multiplier(suffix: char) -> Result<f64, String> {
    match suffix.to_ascii_lowercase() {
        'f' => Ok(1e-15),
        'p' => Ok(1e-12),
        'n' => Ok(1e-9),
        'u' => Ok(1e-6),
        'm' => Ok(1e-3),
        'k' => Ok(1e3),
        'g' => Ok(1e9),
        't' => Ok(1e12),
        _ => Err(format!("unknown suffix '{suffix}'")),
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
}

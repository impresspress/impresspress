//! Money and currency invariants shared by offers, orders, and providers.
//!
//! Commerce amounts are stored and calculated as integer minor units, with
//! currency validation at every boundary.

/// Normalize an ISO-style three-letter currency code for storage/provider use.
///
/// Stripe expects lowercase codes on its form API while ImpressPress stores
/// uppercase codes. This function returns the canonical storage form; callers
/// lowercase only at the provider boundary.
pub fn normalize_currency(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err("currency must be a three-letter ISO code");
    }
    Ok(value.to_ascii_uppercase())
}

/// Return the number of decimal places used for charge amounts in a currency.
///
/// Most currencies have two decimal places. Stripe's zero-decimal and
/// three-decimal charge currencies are handled explicitly; unknown but
/// well-formed ISO codes use the normal two-place representation.
pub fn currency_exponent(currency: &str) -> Result<u32, &'static str> {
    let currency = normalize_currency(currency)?;
    let exponent = match currency.as_str() {
        "BIF" | "CLP" | "DJF" | "GNF" | "JPY" | "KMF" | "KRW" | "MGA" | "PYG" | "RWF" | "UGX"
        | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "JOD" | "KWD" | "OMR" | "TND" => 3,
        _ => 2,
    };
    Ok(exponent)
}

/// Parse an admin/customer decimal string into integer minor units exactly.
///
/// The conversion never passes through a binary floating-point value. Extra
/// fractional places are accepted only when they are zero, so input is never
/// silently rounded. Commerce prices are non-negative; discounts are stored
/// in their own positive amount fields.
pub fn parse_amount_minor(input: &str, currency: &str) -> Result<i64, String> {
    let exponent = currency_exponent(currency).map_err(str::to_string)? as usize;
    let input = input.trim();
    if input.is_empty() {
        return Err("amount is required".to_string());
    }
    if input.starts_with('-') {
        return Err("amount must not be negative".to_string());
    }
    let input = input.strip_prefix('+').unwrap_or(input);
    let mut parts = input.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("amount must be a plain decimal number".to_string());
    }

    let mut fraction = fraction.to_string();
    if fraction.len() > exponent {
        if fraction[exponent..].bytes().any(|byte| byte != b'0') {
            return Err(format!(
                "amount has more than {exponent} decimal places for the currency"
            ));
        }
        fraction.truncate(exponent);
    }
    while fraction.len() < exponent {
        fraction.push('0');
    }

    let whole = if whole.is_empty() { "0" } else { whole };
    let whole = whole
        .parse::<i128>()
        .map_err(|_| "amount is too large".to_string())?;
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| "amount is too large".to_string())?
    };
    let multiplier = 10_i128
        .checked_pow(exponent as u32)
        .ok_or_else(|| "currency exponent is too large".to_string())?;
    let minor = whole
        .checked_mul(multiplier)
        .and_then(|value| value.checked_add(fractional))
        .ok_or_else(|| "amount is too large".to_string())?;
    i64::try_from(minor).map_err(|_| "amount is too large".to_string())
}

/// Format integer minor units for human-facing summaries without floats.
pub fn format_amount_minor(amount_minor: i64, currency: &str) -> Result<String, String> {
    let exponent = currency_exponent(currency).map_err(str::to_string)?;
    if exponent == 0 {
        return Ok(amount_minor.to_string());
    }
    let multiplier = 10_i64.pow(exponent);
    let sign = if amount_minor < 0 { "-" } else { "" };
    let absolute = amount_minor.unsigned_abs();
    Ok(format!(
        "{sign}{}.{:0width$}",
        absolute / multiplier as u64,
        absolute % multiplier as u64,
        width = exponent as usize
    ))
}

#[cfg(test)]
mod tests {
    use super::{currency_exponent, format_amount_minor, normalize_currency, parse_amount_minor};

    #[test]
    fn currency_is_normalized_to_uppercase() {
        assert_eq!(normalize_currency("nzd"), Ok("NZD".to_string()));
        assert_eq!(normalize_currency(" UsD "), Ok("USD".to_string()));
    }

    #[test]
    fn currency_rejects_non_iso_shapes() {
        for invalid in ["", "US", "USDD", "U1D", "€UR", "US D"] {
            assert_eq!(
                normalize_currency(invalid),
                Err("currency must be a three-letter ISO code"),
                "{invalid:?} should be invalid"
            );
        }
    }

    #[test]
    fn decimal_amounts_convert_without_floating_point_rounding() {
        assert_eq!(parse_amount_minor("19.99", "USD"), Ok(1999));
        assert_eq!(parse_amount_minor("0.10", "NZD"), Ok(10));
        assert_eq!(parse_amount_minor(".5", "EUR"), Ok(50));
        assert_eq!(parse_amount_minor("19.9900", "USD"), Ok(1999));
        assert_eq!(format_amount_minor(1999, "USD"), Ok("19.99".to_string()));
        assert_eq!(format_amount_minor(-5, "USD"), Ok("-0.05".to_string()));
    }

    #[test]
    fn decimal_amounts_respect_currency_exponents() {
        assert_eq!(currency_exponent("JPY"), Ok(0));
        assert_eq!(currency_exponent("KWD"), Ok(3));
        assert_eq!(parse_amount_minor("500", "JPY"), Ok(500));
        assert_eq!(parse_amount_minor("1.234", "KWD"), Ok(1234));
        assert!(parse_amount_minor("1.2", "JPY").is_err());
        assert!(parse_amount_minor("1.2345", "KWD").is_err());
    }

    #[test]
    fn decimal_amounts_reject_invalid_or_unsafe_values() {
        for invalid in ["", "-1", "1.001", "1e3", "1.2.3", "NaN"] {
            assert!(parse_amount_minor(invalid, "USD").is_err(), "{invalid}");
        }
        assert!(parse_amount_minor("999999999999999999999", "USD").is_err());
    }
}

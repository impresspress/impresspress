//! Money and currency invariants shared by offers, orders, and providers.
//!
//! Commerce amounts are stored and calculated as integer minor units, with
//! currency validation at every boundary.
//!
//! [`Decimal`] is the **one** decimal type on that path. `offer_pricing.rs`
//! used to define a second one, so the same string had two readings — one
//! that knew about currencies and rejected a fraction the currency cannot
//! express, and one that knew about neither — and `parse_amount_minor` (the
//! currency-aware half) had no production callers at all, so the live
//! pricing path was the half that could not tell dollars from yen. They are
//! one type now: `Decimal::parse` reads the string, `Decimal::to_minor`
//! answers what it is worth in a named currency, and `parse_amount_minor` is
//! those two calls in sequence.

use std::cmp::Ordering;

use serde_json::Value;

/// The most fractional digits a decimal may carry *after* normalisation.
///
/// The cap is on significant places, not written ones: `1.0000000000` is
/// exactly one and is accepted, while `0.0000000001` is not representable
/// here and is refused. Checking before normalisation is what made
/// `Decimal::parse` and [`parse_amount_minor`] disagree about that first
/// string.
const MAX_DECIMAL_SCALE: u32 = 9;

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

/// An exact decimal number, held as `coefficient × 10^-scale` and normalised
/// so it carries no trailing fractional zeros. No value on this path ever
/// passes through a binary floating-point number.
///
/// Normalisation is what makes [`Self::to_minor`] exact: after it, a
/// non-zero `scale` means the last fractional digit is significant, so
/// "does this fit the currency's exponent?" is a comparison of two integers
/// rather than a scan for stray digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Decimal {
    coefficient: i128,
    scale: u32,
}

impl Decimal {
    /// The whole number `value`, exactly.
    pub(super) fn from_integer(value: i64) -> Self {
        Self {
            coefficient: i128::from(value),
            scale: 0,
        }
    }

    /// Read a plain decimal string: an optional sign, digits, an optional
    /// `.` and more digits. Scientific notation, a second `.` and any
    /// non-digit are refused, so nothing here can round or overflow through
    /// a float.
    ///
    /// Negatives parse — refunds, discounts and adjustments are decimals
    /// too — and it is [`Self::to_minor`] that refuses them where an amount
    /// must be non-negative.
    pub(super) fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let (negative, unsigned) = if let Some(value) = input.strip_prefix('-') {
            (true, value)
        } else {
            (false, input.strip_prefix('+').unwrap_or(input))
        };
        let mut parts = unsigned.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || (whole.is_empty() && fraction.is_empty())
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("must be a plain decimal number".to_string());
        }
        let digits = format!("{}{}", if whole.is_empty() { "0" } else { whole }, fraction);
        let mut coefficient = digits
            .parse::<i128>()
            .map_err(|_| "number is too large".to_string())?;
        if negative {
            coefficient = -coefficient;
        }
        let mut value = Self {
            coefficient,
            scale: fraction.len() as u32,
        };
        while value.scale > 0 && value.coefficient % 10 == 0 {
            value.coefficient /= 10;
            value.scale -= 1;
        }
        // After normalisation, not before: `1.0000000000` is exactly one and
        // must read the same as `1`. Checking the written places first is
        // what made this parser refuse a string `parse_amount_minor` read as
        // 100 US cents.
        if value.scale > MAX_DECIMAL_SCALE {
            return Err(format!(
                "must have at most {MAX_DECIMAL_SCALE} decimal places"
            ));
        }
        Ok(value)
    }

    pub(super) fn from_json(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(value) => Self::parse(&value.to_string()),
            Value::String(value) => Self::parse(value),
            _ => Err("must be a number or decimal string".to_string()),
        }
    }

    /// This value's coefficient restated at `scale` fractional places.
    ///
    /// Refuses a `scale` below the value's own rather than answering
    /// anyway. It used `saturating_sub`, which turned a scale it could not
    /// honour into a multiply-by-one — `1.005` "aligned" to two places came
    /// back as `1005`, ten times the value, silently, in the type that
    /// prices offers.
    pub(super) fn aligned(self, scale: u32) -> Result<i128, String> {
        if scale < self.scale {
            return Err(format!("value has more than {scale} decimal places"));
        }
        let multiplier = 10_i128
            .checked_pow(scale - self.scale)
            .ok_or_else(|| "number is too large".to_string())?;
        self.coefficient
            .checked_mul(multiplier)
            .ok_or_else(|| "number is too large".to_string())
    }

    pub(super) fn compare(self, other: Self) -> Result<Ordering, String> {
        let scale = self.scale.max(other.scale);
        Ok(self.aligned(scale)?.cmp(&other.aligned(scale)?))
    }

    pub(super) fn is_step_from(self, base: Self, step: Self) -> Result<bool, String> {
        if step.coefficient <= 0 {
            return Err("step must be greater than zero".to_string());
        }
        let scale = self.scale.max(base.scale).max(step.scale);
        let difference = self
            .aligned(scale)?
            .checked_sub(base.aligned(scale)?)
            .ok_or_else(|| "number is too large".to_string())?;
        Ok(difference % step.aligned(scale)? == 0)
    }

    pub(super) fn multiply_minor(self, amount_minor: i64) -> Result<i64, String> {
        let numerator = self
            .coefficient
            .checked_mul(amount_minor as i128)
            .ok_or_else(|| "calculated amount is too large".to_string())?;
        let denominator = 10_i128
            .checked_pow(self.scale)
            .ok_or_else(|| "number is too large".to_string())?;
        if numerator % denominator != 0 {
            return Err(
                "value does not resolve to a whole minor-unit amount; adjust the value or rate"
                    .to_string(),
            );
        }
        i64::try_from(numerator / denominator)
            .map_err(|_| "calculated amount is too large".to_string())
    }

    pub(super) fn as_u64(self) -> Option<u64> {
        let denominator = 10_i128.checked_pow(self.scale)?;
        if self.coefficient < 0 || self.coefficient % denominator != 0 {
            return None;
        }
        u64::try_from(self.coefficient / denominator).ok()
    }

    pub(super) fn canonical(self) -> String {
        let sign = if self.coefficient < 0 { "-" } else { "" };
        let digits = self.coefficient.unsigned_abs().to_string();
        if self.scale == 0 {
            return format!("{sign}{digits}");
        }
        let scale = self.scale as usize;
        let padded = if digits.len() <= scale {
            format!("{}{}", "0".repeat(scale + 1 - digits.len()), digits)
        } else {
            digits
        };
        let split = padded.len() - scale;
        format!("{sign}{}.{}", &padded[..split], &padded[split..])
    }

    /// This value as a non-negative integer count of `currency`'s minor
    /// units.
    ///
    /// Commerce prices are non-negative — discounts and refunds are stored
    /// in their own positive amount fields — so a negative value is refused
    /// here rather than at every call site. [`Self::to_signed_minor`] is the
    /// one for the ledger paths that genuinely carry a sign.
    pub(super) fn to_minor(self, currency: &str) -> Result<i64, String> {
        if self.coefficient < 0 {
            // Validate the currency first so the message a caller sees
            // does not depend on which fault is noticed first.
            currency_exponent(currency).map_err(str::to_string)?;
            return Err("amount must not be negative".to_string());
        }
        self.to_signed_minor(currency)
    }

    /// [`Self::to_minor`] without the sign check.
    ///
    /// A fractional digit the currency cannot express is a refusal, never a
    /// rounding: `1.005` is not a whole number of US cents, `1.2` is not a
    /// whole number of yen. Because the value is normalised, a `scale`
    /// above the currency's exponent *is* that condition — the digits that
    /// would have been dropped are known to be significant.
    pub(super) fn to_signed_minor(self, currency: &str) -> Result<i64, String> {
        let exponent = currency_exponent(currency).map_err(str::to_string)?;
        if self.scale > exponent {
            return Err(format!(
                "amount has more than {exponent} decimal places for the currency"
            ));
        }
        let minor = self
            .aligned(exponent)
            .map_err(|_| "amount is too large".to_string())?;
        i64::try_from(minor).map_err(|_| "amount is too large".to_string())
    }
}

/// Parse an admin/customer decimal string into integer minor units exactly.
///
/// The two halves of the one decimal path: read the string, then ask what it
/// is worth in this currency. Both halves are [`Decimal`]'s, so the pricing
/// engine and this function can no longer answer differently.
pub fn parse_amount_minor(input: &str, currency: &str) -> Result<i64, String> {
    // The currency is checked even for an unparseable amount, so a bad
    // currency is reported as a bad currency whichever argument is wrong.
    currency_exponent(currency).map_err(str::to_string)?;
    Decimal::parse(input)?.to_minor(currency)
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
    use super::{
        currency_exponent, format_amount_minor, normalize_currency, parse_amount_minor, Decimal,
    };

    /// The block carried two decimal parsers, both on the money path, and
    /// they read the same string differently.
    ///
    /// `1.0000000000` is exactly one. `parse_amount_minor` read it as 100 US
    /// cents; `offer_pricing`'s `Decimal` refused it outright, because it
    /// counted decimal places *before* normalising the trailing zeros away.
    /// One type now, one answer.
    #[test]
    fn the_pricing_parser_reads_a_money_string_the_way_money_rs_does() {
        assert_eq!(parse_amount_minor("1.0000000000", "USD"), Ok(100));
        assert_eq!(
            Decimal::parse("1.0000000000").map(Decimal::canonical),
            Ok("1".to_string()),
            "ten trailing zeros are still exactly one; the scale cap belongs \
             after normalisation, not before it"
        );
        assert_eq!(
            Decimal::parse("1.0000000000").and_then(|value| value.to_minor("USD")),
            parse_amount_minor("1.0000000000", "USD"),
            "the pricing path and the money path are the same two calls now"
        );
    }

    /// A tenth of a cent is not representable and never was; the cap is on
    /// *significant* places, so relaxing it for trailing zeros does not
    /// relax it for real precision.
    #[test]
    fn nine_significant_decimal_places_is_still_the_limit() {
        assert!(Decimal::parse("0.000000001").is_ok());
        assert!(Decimal::parse("0.0000000001").is_err());
        assert!(Decimal::parse("1.2345678901").is_err());
    }

    /// The reason the second parser could not simply be deleted: it had no
    /// notion of a currency, so nothing on the live pricing path could say
    /// how many minor units a decimal is worth. `to_minor` is that answer,
    /// and `parse_amount_minor` is now expressed on it.
    #[test]
    fn to_minor_respects_the_currency_exponent() {
        for (input, currency, expected) in [
            ("19.99", "USD", 1999),
            ("0.10", "NZD", 10),
            (".5", "EUR", 50),
            ("19.9900", "USD", 1999),
            ("500", "JPY", 500),
            ("1.234", "KWD", 1234),
        ] {
            let value = Decimal::parse(input).expect(input);
            assert_eq!(
                value.to_minor(currency),
                Ok(expected),
                "{input} in {currency}"
            );
            assert_eq!(
                parse_amount_minor(input, currency),
                Ok(expected),
                "{input} in {currency}, through the money entry point"
            );
        }
    }

    /// A digit the currency cannot express is a refusal, not a rounding —
    /// and which digits those are depends on the currency, which is exactly
    /// what the pricing engine's own parser could not know.
    #[test]
    fn to_minor_refuses_precision_the_currency_cannot_express() {
        assert!(Decimal::parse("1.005").unwrap().to_minor("USD").is_err());
        assert!(Decimal::parse("1.2").unwrap().to_minor("JPY").is_err());
        assert!(Decimal::parse("1.2345").unwrap().to_minor("KWD").is_err());
        // The same values where the currency *can* express them.
        assert_eq!(Decimal::parse("1.005").unwrap().to_minor("KWD"), Ok(1005));
        assert_eq!(Decimal::parse("1.2").unwrap().to_minor("USD"), Ok(120));
    }

    /// Negatives parse — refunds and adjustments are decimals too — and it
    /// is the conversion, not the parse, that refuses them where an amount
    /// must be non-negative.
    #[test]
    fn a_negative_parses_but_is_not_an_amount() {
        let value = Decimal::parse("-1.50").expect("a negative decimal parses");
        assert!(value.to_minor("USD").is_err());
        assert_eq!(value.to_signed_minor("USD"), Ok(-150));
        assert!(parse_amount_minor("-1.50", "USD").is_err());
    }

    /// `aligned` is the decimal-to-fixed-scale conversion everything else is
    /// built on. It used `saturating_sub`, so a scale it could not honour
    /// became a silent multiply-by-one: `1.005` aligned to two places came
    /// back as `1005`, ten times the value, in the type that prices offers.
    #[test]
    fn aligning_below_a_value_s_own_scale_is_refused_not_silently_multiplied() {
        let value = Decimal::parse("1.005").expect("parses");
        assert_eq!(value.aligned(3), Ok(1005));
        assert!(
            value.aligned(2).is_err(),
            "1.005 does not fit two decimal places; answering 1005 is a 10x error"
        );
    }

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

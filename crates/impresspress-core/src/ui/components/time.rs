//! Timestamps as the tables show them.

use maud::{html, Markup};

/// An RFC 3339 timestamp as a `<time>` element: [`datetime_attr`]'s
/// machine-readable form in `datetime`, and [`crate::util::format_timestamp`]'s
/// minute-precision UTC text for people (`2026-05-06 10:00`, not
/// `2026-05-06T10:00:00.123456789Z`).
///
/// A `<time>` element because it is one, and because the visual-baseline suite
/// masks `time`: a value that differs on every run must be one.
pub fn timestamp(rfc3339: &str) -> Markup {
    html! { time datetime=(datetime_attr(rfc3339)) { (crate::util::format_timestamp(rfc3339)) } }
}

/// The value of a `<time datetime>` for a stored RFC 3339 timestamp: UTC with
/// millisecond precision (`2026-05-06T10:00:00.123Z`). HTML's global date and
/// time string allows one to three fractional-second digits, and the stored
/// values carry nine. A value that does not parse is passed through, so the
/// element still carries what was stored.
fn datetime_attr(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        Err(_) => rfc3339.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_carries_a_millisecond_datetime_and_the_humanised_text() {
        assert_eq!(
            super::timestamp("2026-05-06T10:00:00.123456789Z").into_string(),
            r#"<time datetime="2026-05-06T10:00:00.123Z">2026-05-06 10:00</time>"#
        );
    }

    #[test]
    fn datetime_is_utc_with_three_fraction_digits_or_the_raw_value() {
        let cases = [
            ("2026-05-06T10:00:00Z", "2026-05-06T10:00:00.000Z"),
            ("2026-05-06T12:30:00.5+02:00", "2026-05-06T10:30:00.500Z"),
            ("not a date", "not a date"),
        ];
        for (raw, want) in cases {
            assert_eq!(super::datetime_attr(raw), want, "{raw}");
        }
    }
}

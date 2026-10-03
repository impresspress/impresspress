//! Date-times as the admin shows them.

use maud::{html, Markup};

/// A stored date-time as people read it: `2026-10-03 05:43 UTC` — converted
/// to UTC, minute precision, the same digits in every locale, and the zone
/// stated in the text (one convention for every table: a suffix on the value,
/// not a note in a column header a caller could forget). It sits in a
/// `<time>` element whose `datetime` and `title` carry the instant in UTC to
/// the millisecond (`2026-10-03T05:43:12.123Z`) — finer than the cell, though
/// a stored value with more fractional digits is truncated to three, and one
/// stored with an offset is shown converted.
///
/// The one helper every curated table and card uses for a timestamp, rather
/// than printing the stored ISO string or slicing it (`created.get(..19)`),
/// which left a `T` in the middle and dropped the zone. The text is set in
/// tabular figures on one line (`.datetime` in `table.css`), so a column of
/// them aligns and never breaks between the date and the time. (The SQL
/// explorer is not curated: it prints values as stored.)
///
/// Accepts RFC 3339 (`2026-10-03T05:43:12.123456789Z`, any offset), and a
/// zone-less `YYYY-MM-DD[T ]HH:MM[:SS[.f]]` read as UTC (what SQLite's own
/// `datetime()` writes). A value that is neither is printed as stored (in a
/// `span`, since a `<time>` must carry a valid date), so nothing is hidden.
///
/// A `<time>` element because it is one, and because the visual-baseline
/// suite masks `time`: a value that differs on every run must be one.
pub fn timestamp(raw: &str) -> Markup {
    match parse_utc(raw) {
        Some(dt) => {
            let iso = dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            html! {
                time .datetime datetime=(iso) title=(iso) { (dt.format("%Y-%m-%d %H:%M")) " UTC" }
            }
        }
        None => html! { span .datetime { (raw) } },
    }
}

/// `raw` as a UTC date-time: RFC 3339 with any offset, or a zone-less
/// date-time taken to be UTC.
fn parse_utc(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|fmt| chrono::NaiveDateTime::parse_from_str(raw, fmt).ok())
    .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_renders_minutes_with_the_full_iso_in_datetime_and_title() {
        assert_eq!(
            timestamp("2026-10-03T05:43:12.123456789Z").into_string(),
            r#"<time class="datetime" datetime="2026-10-03T05:43:12.123Z" title="2026-10-03T05:43:12.123Z">2026-10-03 05:43 UTC</time>"#
        );
    }

    #[test]
    fn timestamp_normalises_every_accepted_shape_to_utc() {
        let cases = [
            (
                "2026-05-06T10:00:00Z",
                "2026-05-06T10:00:00.000Z",
                "2026-05-06 10:00",
            ),
            (
                "2026-05-06T12:30:00.5+02:00",
                "2026-05-06T10:30:00.500Z",
                "2026-05-06 10:30",
            ),
            // Zone-less values are UTC (SQLite's `datetime()` shape too).
            (
                "2026-05-06T10:00:00",
                "2026-05-06T10:00:00.000Z",
                "2026-05-06 10:00",
            ),
            (
                "2026-05-06 10:00:59.9",
                "2026-05-06T10:00:59.900Z",
                "2026-05-06 10:00",
            ),
        ];
        for (raw, iso, text) in cases {
            assert_eq!(
                timestamp(raw).into_string(),
                format!(
                    r#"<time class="datetime" datetime="{iso}" title="{iso}">{text} UTC</time>"#
                ),
                "{raw}"
            );
        }
    }

    #[test]
    fn an_unparseable_value_is_printed_as_stored() {
        assert_eq!(
            timestamp("not a date").into_string(),
            r#"<span class="datetime">not a date</span>"#
        );
    }
}

//! Stat row

use maud::{html, Markup};

/// The most stats one row holds. A seventh figure is not a headline: it
/// belongs in a key/value list under the row.
pub const MAX_STATS: usize = 6;

/// One headline figure: a sentence-case label and its value, with an
/// optional line that breaks the value down.
///
/// A stat is a card-level surface, so it sits on the page background in a
/// [`stat_row`], not inside a `Card` (layout rule 2). It has no icon and no
/// sparkline: the label says what is counted and the value is the figure.
#[must_use = "a Stat renders nothing until it is passed to `stat_row`"]
pub struct Stat<'a> {
    label: &'a str,
    value: &'a str,
    detail: Option<Markup>,
    alert: bool,
}

impl<'a> Stat<'a> {
    /// `label` is rendered as written, so write it in sentence case
    /// ("Total users"); no stylesheet changes its case.
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            detail: None,
            alert: false,
        }
    }

    /// One line under the value that breaks it down ("12 client errors").
    pub fn detail(mut self, detail: Option<Markup>) -> Self {
        self.detail = detail;
        self
    }

    /// Draw the value in the danger tone: for a figure that is a problem
    /// whenever it is not zero. The label already says what is counted, so
    /// the colour is never the only signal.
    pub fn alert(mut self, alert: bool) -> Self {
        self.alert = alert;
        self
    }

    fn render(self) -> Markup {
        html! {
            div class=(if self.alert { "stat stat--alert" } else { "stat" }) {
                div .stat__label { (self.label) }
                div .stat__value { (self.value) }
                @if let Some(detail) = self.detail { div .stat__detail { (detail) } }
            }
        }
    }
}

/// One compact row of [`Stat`]s: `div.stat-row > div.stat-row__grid > div.stat`.
///
/// The layout follows the width the row itself is given, not the viewport
/// (`.stat-row` is a CSS size container), so a row in a card and a row on
/// the page background each pick their own:
/// - every stat in one row of equal columns when each gets the stat's
///   minimum width;
/// - otherwise two columns, an odd last stat spanning both, so no stat is
///   left alone beside an empty column;
/// - one column when even two would be too narrow.
///
/// Each stat is at least 11rem wide, which leaves 142px for its value at
/// 1.25rem/600. A value word that fits that (DejaVu Sans, the font CI
/// renders) never breaks: today the widest is the dashboard's outage marker
/// "Unavailable", at 132.5px. Anything wider breaks mid-word, so a marker
/// must fit or wrap at a space ("Not configured"), and a figure must be at
/// most about ten characters: an eleven-character amount such as
/// "12345678.90", or a ten-digit JPY amount, would break.
///
/// Holds one to [`MAX_STATS`] stats.
pub fn stat_row(stats: Vec<Stat<'_>>) -> Markup {
    assert!(
        (1..=MAX_STATS).contains(&stats.len()),
        "a stat row holds 1 to {MAX_STATS} stats, got {}; put the rest in a key/value list",
        stats.len()
    );
    html! {
        div .stat-row {
            div .stat-row__grid {
                @for stat in stats { (stat.render()) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dom;

    #[test]
    fn a_stat_is_a_label_then_its_value_with_no_icon() {
        let html = stat_row(vec![Stat::new("Total users", "142")]).into_string();
        assert_eq!(
            html,
            r#"<div class="stat-row"><div class="stat-row__grid"><div class="stat"><div class="stat__label">Total users</div><div class="stat__value">142</div></div></div></div>"#
        );
    }

    #[test]
    fn the_label_is_rendered_as_written_and_escaped() {
        let html = stat_row(vec![Stat::new("Q&A <today>", "1 & 2")]).into_string();
        assert!(
            html.contains(r#"<div class="stat__label">Q&amp;A &lt;today&gt;</div>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="stat__value">1 &amp; 2</div>"#),
            "{html}"
        );
    }

    #[test]
    fn a_detail_renders_under_the_value() {
        let html = stat_row(vec![
            Stat::new("Requests today", "4").detail(Some(html! { "2 client errors" }))
        ])
        .into_string();
        assert!(
            html.contains(
                r#"<div class="stat__value">4</div><div class="stat__detail">2 client errors</div>"#
            ),
            "{html}"
        );
        let none = stat_row(vec![Stat::new("Requests today", "4").detail(None)]).into_string();
        assert!(!none.contains("stat__detail"), "{none}");
    }

    #[test]
    fn an_alert_stat_carries_the_alert_modifier() {
        let html = stat_row(vec![
            Stat::new("Server errors today", "1").alert(true),
            Stat::new("Avg response", "4ms").alert(false),
        ])
        .into_string();
        assert_eq!(html.matches(r#"<div class="stat stat--alert">"#).count(), 1);
        assert_eq!(html.matches(r#"<div class="stat">"#).count(), 1);
    }

    #[test]
    fn the_row_keeps_the_stats_in_order() {
        let html = stat_row(vec![
            Stat::new("Buckets", "1"),
            Stat::new("Files", "2"),
            Stat::new("Total size", "3 B"),
        ])
        .into_string();
        let at = |label: &str| html.find(&format!(">{label}<")).unwrap();
        assert!(at("Buckets") < at("Files") && at("Files") < at("Total size"));
        assert_eq!(html.matches(r#"class="stat""#).count(), 3);
    }

    /// The stats are card-level surfaces side by side; none is inside
    /// another, and the row adds no heading.
    #[test]
    fn a_row_holds_no_nested_card_level_surface_and_no_heading() {
        let html = stat_row(vec![Stat::new("Buckets", "1"), Stat::new("Files", "2")]).into_string();
        assert!(dom::card_level_inside_card(&html).is_empty(), "{html}");
        assert!(dom::heading_outline(&html).is_empty(), "{html}");
    }

    #[test]
    fn six_stats_fit_in_one_row() {
        let labels = ["a", "b", "c", "d", "e", "f"];
        let html = stat_row(labels.iter().map(|l| Stat::new(l, "1")).collect()).into_string();
        assert_eq!(html.matches(r#"class="stat""#).count(), MAX_STATS);
    }

    /// The rows the layout spec (`stat-row.spec.ts`) lays out in a real
    /// browser, with values as the pages format them: the dashboard (five),
    /// the dashboard during an outage (five "Unavailable" markers), an order
    /// (three, money and a count), a seller's state words (four, the longest
    /// real ones) and a single stat. The spec reads them from
    /// a fixture file so it runs against `stat_row`'s markup rather than a
    /// copy of it; this keeps the file what `stat_row` renders today. To
    /// refresh it after a deliberate markup change, write `stat_row_fixture()`
    /// to the path below.
    fn stat_row_fixture() -> String {
        let dashboard = stat_row(vec![
            Stat::new("Total users", "1204"),
            Stat::new("New today", "12"),
            Stat::new("Requests today", "48210")
                .detail(Some(html! { "2 client errors · 1 rate-limited" })),
            Stat::new("Server errors today", "1").alert(true),
            Stat::new("Avg response", "143ms"),
        ]);
        let outage = stat_row(
            [
                "Total users",
                "New today",
                "Requests today",
                "Server errors today",
                "Avg response",
            ]
            .map(|label| Stat::new(label, "Unavailable"))
            .into(),
        );
        let order = stat_row(vec![
            Stat::new("Total", "12345.67 USD"),
            Stat::new("Refunded", "1000.00 USD"),
            Stat::new("Items", "3"),
        ]);
        let seller = stat_row(vec![
            Stat::new("Payments", "Disabled"),
            Stat::new("Payouts", "Disabled"),
            Stat::new("Verification", "Incomplete"),
            Stat::new("Platform fee", "Not configured"),
        ]);
        let single = stat_row(vec![Stat::new("Purchases", "27")]);
        [dashboard, outage, order, seller, single]
            .map(|row| format!("{}\n", row.into_string()))
            .concat()
    }

    #[test]
    fn the_stat_row_e2e_fixture_is_what_stat_row_renders() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/stat-row.html"
        );
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk,
            stat_row_fixture(),
            "{path} is out of date; write the expected text above to it"
        );
    }

    #[test]
    #[should_panic(expected = "a stat row holds 1 to 6 stats, got 7")]
    fn a_seventh_stat_is_refused() {
        let labels = ["a", "b", "c", "d", "e", "f", "g"];
        let _ = stat_row(labels.iter().map(|l| Stat::new(l, "1")).collect());
    }

    #[test]
    #[should_panic(expected = "a stat row holds 1 to 6 stats, got 0")]
    fn an_empty_row_is_refused() {
        let _ = stat_row(Vec::new());
    }
}

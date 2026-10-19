//! Dashboard charts: the 30-day line and bar chart cards.
//!
//! Both chart kinds are one component ([`ChartCard`]) with two plot kinds,
//! so they share the value axis, the gridlines, the per-day tooltips, the
//! date range, the history note and the accessible table of
//! values. The drawing is decorative (`aria-hidden`): what a screen reader,
//! a keyboard or a touch user reads is the "Show values" disclosure under it,
//! a real `<table>` with a caption.
//!
//! A chart is drawn only when its series has data (layout rule 10). A window
//! that is zero every day renders one [`empty_note`] sentence in place of the
//! drawing, so a fresh install shows four short sentences rather than four
//! flat lines.

use maud::{html, Markup};

use super::{empty_note, Card};

/// How a [`ChartCard`] draws its series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plot {
    /// A line with a shaded area under it and a dot on the last day.
    Line,
    /// One column per day.
    Bars,
}

/// A 30-day chart card: a titled [`Card`] with a head link, holding the
/// drawing of `data`, or the sentence that says it is zero throughout.
///
/// `data` is ordered chronologically, one `(YYYY-MM-DD, count)` per day.
/// Render it as a line ([`line`](ChartCard::line)) or as columns
/// ([`bars`](ChartCard::bars)); both share the same value axis.
pub struct ChartCard<'a> {
    /// The card's `h2`: "Server errors".
    pub title: &'a str,
    /// The window, under the title: "Last 30 days".
    pub subtitle: &'a str,
    /// What the series counts, as running text ("server errors"). It names
    /// the zero in an all-zero window: "No server errors in the last 30
    /// days". Written out rather than derived from `title`, which would lose
    /// an acronym's case to a lowercase.
    pub counts: &'a str,
    pub data: &'a [(String, i64)],
    pub history: ChartHistory,
    /// The CSS colour of the drawing: `var(--accent-danger)`.
    pub color_var: &'a str,
    /// The head link as `(label, href)`. The label is the link's accessible
    /// name, so it names the destination ("View server errors"), not just
    /// the verb.
    pub link: (&'a str, &'a str),
}

impl ChartCard<'_> {
    /// Draw the series as a line with a shaded area under it.
    pub fn line(self) -> Markup {
        chart_card(Plot::Line, self)
    }

    /// Draw the series as one column per day.
    pub fn bars(self) -> Markup {
        chart_card(Plot::Bars, self)
    }
}

/// How far back the data behind a chart's series goes — which is not the
/// same as its first non-zero day: a day with zero requests after logging
/// began is a measurement, a day before it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartHistory {
    /// Records exist from this day on (the day of the oldest one).
    Since(chrono::NaiveDate),
    /// Nothing has been recorded yet.
    Empty,
    /// Could not be read; the card makes no claim about it.
    Unknown,
}

/// A day as the chart labels it: `Sep 3`.
fn short_day(day: &str) -> String {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d.format("%b %-d").to_string())
        .unwrap_or_else(|_| day.to_string())
}

/// How many days of the window the records cover, when the records start
/// inside it: `Some(n)` with `n` below the window's length. `None` when they
/// cover it all, or when the history makes no claim.
fn days_so_far(data: &[(String, i64)], history: ChartHistory) -> Option<usize> {
    let ChartHistory::Since(first) = history else {
        return None;
    };
    let last = data
        .last()
        .and_then(|(d, _)| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())?;
    let covered = ((last - first).num_days() + 1).max(1) as usize;
    (covered < data.len()).then_some(covered)
}

/// `"1 day"` / `"3 days"`.
fn days(n: usize) -> String {
    if n == 1 {
        "1 day".to_string()
    } else {
        format!("{n} days")
    }
}

/// The note above a drawn chart, or `None` when the series speaks for itself.
///
/// Records reaching back fewer days than the window: "Collecting data — N
/// days so far", because the flat line before them is not data. `Empty`
/// meets a series with data only across a race between the two reads, and
/// `Unknown` makes no claim; neither adds a note.
fn history_note(data: &[(String, i64)], history: ChartHistory) -> Option<String> {
    days_so_far(data, history).map(|n| format!("Collecting data — {} so far", days(n)))
}

/// The sentence a chart card holds in place of a series that is zero every
/// day. Zero is data once recording has begun, so the sentence says which:
///
/// - Nothing recorded yet: "No server errors recorded yet".
/// - Records reach back fewer days than the window: "No server errors yet —
///   collecting data, N days so far", because the days before them were not
///   measured.
/// - Otherwise: "No server errors in the last 30 days".
fn zero_note(counts: &str, data: &[(String, i64)], history: ChartHistory) -> String {
    if history == ChartHistory::Empty {
        return format!("No {counts} recorded yet");
    }
    match days_so_far(data, history) {
        Some(n) => format!("No {counts} yet — collecting data, {} so far", days(n)),
        None => format!("No {counts} in the last {} days", data.len()),
    }
}

/// The shared chart card: the drawing when the series has data, else the
/// [`zero_note`] sentence under the same head.
///
/// The plot, its gridlines, its y-axis labels, its bars and its endpoint dot
/// are all placed from one [`ValueAxis`]: a gridline and its label share the
/// same fraction of the plot's height, so a label cannot sit beside another
/// tick's line, and a bar reaches exactly its value.
///
/// Over the plot sits one transparent column per day carrying that day's
/// tooltip (`data-tooltip`). The tooltip is out of layout until shown
/// (`display: none`), so it never widens the page, and it shows on hover and
/// on focus — the columns take focus from a tap (`tabindex="-1"`) without
/// adding thirty tab stops. Keyboard and screen-reader users get the same
/// values from the "Show values" table.
fn chart_card(plot: Plot, card: ChartCard<'_>) -> Markup {
    let ChartCard {
        title,
        subtitle,
        counts,
        data,
        history,
        color_var,
        link: (link_label, link_href),
    } = card;
    let head = |body: Markup| {
        Card::titled(title, body)
            .subtitle(subtitle)
            .link(link_label, link_href)
            .render()
    };
    if data.iter().all(|(_, v)| *v == 0) {
        return head(empty_note(&zero_note(counts, data, history), None));
    }
    let axis = ValueAxis::for_max(data.iter().map(|(_, v)| *v).max().unwrap_or(0));
    // Distance from the top of the plot as a percentage, the unit the labels
    // and the dot are positioned in; the SVG uses the same value scaled to its
    // 60-unit-tall viewBox.
    let from_top = |value: i64| (1.0 - axis.fraction(value)) * 100.0;
    let ticks = axis.ticks();
    let n = data.len();
    let note = history_note(data, history);
    // The line's points, in the 100x60 viewBox (unused by a bar chart).
    let step = if n > 1 { 100.0 / (n - 1) as f64 } else { 0.0 };
    let line = data
        .iter()
        .enumerate()
        .map(|(i, (_, v))| format!("{:.2},{:.2}", i as f64 * step, from_top(*v) * 0.6))
        .collect::<Vec<_>>()
        .join(" ");
    let body = html! {
        @if let Some(note) = &note {
            p .chart__note { (note) }
        }
        // The drawing. Decorative: the table below carries the values.
        div .chart aria-hidden="true" {
            // Every label is stacked in the axis's one grid cell and
            // moved down to its gridline by `--tick-y`, so the column
            // is as wide as the widest label and each label is centred
            // on its line (see `.chart__ytick` in chart.css).
            div .chart__yaxis {
                @for tick in &ticks {
                    span .chart__ytick style=(format!("--tick-y: {:.2}%", from_top(*tick))) { (tick) }
                }
            }
            // `--chart-color` is declared on this wrapper (not the <svg>
            // itself) so it's visible to the plot, the bars and the
            // `.chart__dot` — a CSS custom property only inherits to
            // descendants of the element it's set on.
            div .chart__plot-wrap style=(format!("--chart-color: {color_var}")) {
                svg .chart__plot viewBox="0 0 100 60" preserveAspectRatio="none" {
                    // maud's `;` void-element syntax emits `<tag attrs>` with no
                    // closing tag for *any* element name — it isn't restricted to
                    // real HTML5 void elements. Browsers require SVG shape elements
                    // (line/polygon/polyline/circle) to be explicitly closed; without
                    // that, each of these becomes a nested *child* of the previous
                    // one instead of a sibling, and browsers refuse to paint shape
                    // elements nested inside another shape element — only the first
                    // gridline would render. `{}` (an empty block body) generates a
                    // matched `<tag></tag>` pair, keeping them proper siblings.
                    @for tick in &ticks {
                        @let y = format!("{:.2}", from_top(*tick) * 0.6);
                        line .chart__gridline x1="0" x2="100" y1=(y) y2=(y) {}
                    }
                    @if plot == Plot::Line {
                        polygon .chart__area points=(format!("0,60 {line} 100,60")) {}
                        polyline .chart__line points=(line) fill="none" {}
                    }
                }
                // The endpoint dot is an HTML div positioned with CSS, not an
                // SVG <circle>. `viewBox="0 0 100 60"` with
                // preserveAspectRatio="none" scales x and y by different
                // factors depending on the rendered box size, which distorts a
                // circle's *fill geometry* into an ellipse — vector-effect only
                // preserves stroke width, it does not help here. A circular div
                // positioned by percentage over the plot stays circular at any
                // width. `--dot-y`, like the labels' `--tick-y`, is a dynamic
                // runtime value passed as a custom property — the only thing
                // these inline styles carry.
                @if plot == Plot::Line {
                    @if let Some((_, last)) = data.last() {
                        div .chart__dot style=(format!("--dot-y: {:.2}%", from_top(*last))) {}
                    }
                }
                // One column per day: the bar (for a bar chart) and the
                // hover/tap target for that day's tooltip. Columns in the
                // outer fifths anchor their tooltip to their own edge so
                // it stays inside the card.
                div .chart__cols {
                    @for (i, (day, val)) in data.iter().enumerate() {
                        @let edge = if i * 5 < n { " chart__col--start" } else if (i + 1) * 5 > n * 4 { " chart__col--end" } else { "" };
                        div class={ "chart__col" (edge) } tabindex="-1" data-tooltip=(format!("{}: {val}", short_day(day))) {
                            @if plot == Plot::Bars {
                                div .chart__bar style=(format!("--size: {:.4}", axis.fraction(*val))) {}
                            }
                        }
                    }
                }
            }
        }
        (date_range(data))
        details .chart__values {
            summary { "Show values" }
            table .chart__table {
                caption { (title) ", " (subtitle.to_lowercase()) }
                thead { tr { th scope="col" { "Day" } th scope="col" { "Count" } } }
                tbody {
                    @for (day, val) in data {
                        tr {
                            th scope="row" { time datetime=(day) { (short_day(day)) } }
                            td { (val) }
                        }
                    }
                }
            }
        }
    };
    head(body)
}

/// The first / last date labels under a 30-day chart.
///
/// Each label is a `<time>` carrying the ISO day, because it is a date: the
/// window ends today, so the text changes every day, and the visual-baseline
/// suite masks `time` elements (and this whole row) for exactly that reason.
/// Decorative, like the drawing above it: the values table carries the days.
fn date_range(data: &[(String, i64)]) -> Markup {
    let label = |day: Option<&String>| -> Markup {
        match day {
            Some(day) => html! { time datetime=(day) { (short_day(day)) } },
            None => html! { span {} },
        }
    };
    html! {
        div .chart__range aria-hidden="true" {
            (label(data.first().map(|(d, _)| d)))
            (label(data.last().map(|(d, _)| d)))
        }
    }
}

/// The value axis of a [`ChartCard`]'s drawing: integer ticks from 0 to `top`,
/// `step` apart, where every tick sits on its own gridline.
#[derive(Debug, PartialEq, Eq)]
struct ValueAxis {
    /// The value at the top of the plot. 0 only for a series that is zero
    /// throughout, which [`chart_card`] never draws: it says so in a
    /// sentence instead.
    top: i64,
    /// The distance between two ticks; 0 exactly when `top` is.
    step: i64,
}

/// At most this many intervals between the baseline and the top tick, so the
/// axis stays readable in a 180px-tall card.
const MAX_AXIS_INTERVALS: i64 = 4;

impl ValueAxis {
    /// The axis for a series whose largest value is `max`.
    ///
    /// The step is the smallest of 1, 2, 5, 10, 20, 50, … that covers `max` in
    /// at most [`MAX_AXIS_INTERVALS`] intervals, and `top` is the first multiple
    /// of it at or above `max`. Every tick is therefore a distinct whole number
    /// placed at its true height, and the axis never labels a value above the
    /// data it has no reason to show: a series that peaks at 1 gets the ticks
    /// 1 and 0, not a padded axis the data never reaches.
    fn for_max(max: i64) -> Self {
        if max <= 0 {
            return ValueAxis { top: 0, step: 0 };
        }
        let mut magnitude: i64 = 1;
        loop {
            for factor in [1, 2, 5] {
                let step = factor * magnitude;
                let intervals = (max + step - 1) / step;
                if intervals <= MAX_AXIS_INTERVALS {
                    return ValueAxis {
                        top: intervals * step,
                        step,
                    };
                }
            }
            magnitude *= 10;
        }
    }

    /// The tick values from the top of the axis down to 0.
    fn ticks(&self) -> Vec<i64> {
        if self.step == 0 {
            return vec![0];
        }
        (0..=self.top / self.step)
            .rev()
            .map(|i| i * self.step)
            .collect()
    }

    /// How far up the plot `value` sits: 0.0 at the baseline, 1.0 at `top`.
    fn fraction(&self, value: i64) -> f64 {
        if self.top == 0 {
            0.0
        } else {
            value as f64 / self.top as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChartCard, ChartHistory};

    /// A "New users" chart card over `data`, ready to draw.
    fn card(data: &[(String, i64)], history: ChartHistory) -> ChartCard<'_> {
        ChartCard {
            title: "New users",
            subtitle: "Last 30 days",
            counts: "new users",
            data,
            history,
            color_var: "var(--x)",
            link: ("View users", "/b/admin/users"),
        }
    }

    /// Both drawings of `data`: the line, then the bars.
    fn both(data: &[(String, i64)], history: ChartHistory) -> [String; 2] {
        [
            card(data, history).line().into_string(),
            card(data, history).bars().into_string(),
        ]
    }

    #[test]
    fn line_chart_card_has_gridlines_and_axis_labels() {
        let data = vec![("2026-08-01".to_string(), 1), ("2026-08-02".to_string(), 3)];
        let m = card(&data, ChartHistory::Unknown).line().into_string();
        assert!(m.contains("chart__gridline"), "gridlines missing");
        assert!(m.contains("chart__ytick"), "y-axis ticks missing");
        assert!(
            m.contains("Aug 1") && m.contains("Aug 2"),
            "x range labels missing"
        );
        assert!(m.contains("chart__dot"), "endpoint dot missing");
    }

    /// The range labels move with the calendar, so both chart kinds must emit
    /// them as `<time>` — the element the visual-baseline suite masks. A plain
    /// `<span>` here makes the dashboard screenshot change every day.
    #[test]
    fn chart_date_range_labels_are_time_elements() {
        let data = vec![("2026-08-27".to_string(), 0), ("2026-09-25".to_string(), 4)];
        for m in both(&data, ChartHistory::Unknown) {
            assert!(
                m.contains(r#"<time datetime="2026-08-27">Aug 27</time>"#)
                    && m.contains(r#"<time datetime="2026-09-25">Sep 25</time>"#),
                "range labels are not <time> elements: {m}"
            );
        }
    }

    fn days(values: &[i64]) -> Vec<(String, i64)> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("2026-09-{:02}", i + 1), *v))
            .collect()
    }

    /// Both chart kinds are a titled `Card`: the h2 is the series, the
    /// subtitle is the window, and the head link is a link named for where
    /// it goes, not a button. Nothing card-level sits inside it (the values
    /// table is a `.chart__table`, not a framed data table).
    #[test]
    fn a_chart_card_is_a_titled_card_with_a_head_link() {
        for m in both(&days(&[1, 3]), ChartHistory::Unknown) {
            assert!(
                m.starts_with(r#"<section class="card"><header class="card__head">"#),
                "{m}"
            );
            assert!(
                m.contains(r#"<p class="card__subtitle">Last 30 days</p>"#),
                "{m}"
            );
            assert!(
                m.contains(r#"<a class="card__link" href="/b/admin/users">View users<svg"#),
                "{m}"
            );
            assert!(!m.contains("btn"), "{m}");
            assert_eq!(
                crate::test_support::dom::heading_outline(&m),
                vec![(2, "New users".to_string())]
            );
            assert!(
                crate::test_support::dom::card_level_inside_card(&m).is_empty(),
                "{m}"
            );
        }
    }

    /// The drawing is decorative; the values are a real table with a
    /// caption, inside a disclosure keyboard and touch users can open.
    #[test]
    fn chart_values_are_an_accessible_table_and_the_drawing_is_hidden() {
        for m in both(&days(&[1, 3]), ChartHistory::Unknown) {
            assert!(
                m.contains(r#"<div class="chart" aria-hidden="true">"#),
                "{m}"
            );
            assert!(
                m.contains(r#"<div class="chart__range" aria-hidden="true">"#),
                "{m}"
            );
            assert!(
                m.contains(r#"<details class="chart__values"><summary>Show values</summary><table class="chart__table"><caption>New users, last 30 days</caption>"#),
                "{m}"
            );
            assert!(
                m.contains(r#"<tr><th scope="row"><time datetime="2026-09-02">Sep 2</time></th><td>3</td></tr>"#),
                "{m}"
            );
        }
    }

    /// One tooltip column per day, focusable by a tap but not a tab stop,
    /// with the outer fifths anchored to their own edge.
    #[test]
    fn every_day_has_a_tap_focusable_tooltip_column() {
        let data = days(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let m = card(&data, ChartHistory::Unknown).line().into_string();
        assert_eq!(m.matches(r#"tabindex="-1""#).count(), 10, "{m}");
        assert!(
            m.contains(r#"<div class="chart__col chart__col--start" tabindex="-1" data-tooltip="Sep 1: 0">"#),
            "{m}"
        );
        assert!(
            m.contains(r#"<div class="chart__col" tabindex="-1" data-tooltip="Sep 5: 4">"#),
            "{m}"
        );
        assert!(
            m.contains(
                r#"<div class="chart__col chart__col--end" tabindex="-1" data-tooltip="Sep 10: 9">"#
            ),
            "{m}"
        );
        assert!(!m.contains("chart__bar"), "a line chart draws no bars: {m}");
    }

    /// The bar chart is drawn against the same axis as the line chart: the
    /// same ticks and gridlines, and each bar's height is its value's
    /// fraction of the axis top (not of the series maximum).
    #[test]
    fn bar_chart_shares_the_line_chart_axis() {
        let data = days(&[0, 3, 5]);
        let [line, bars] = both(&data, ChartHistory::Unknown);
        assert_eq!(axis_of(&bars), axis_of(&line));
        assert_eq!(
            axis_of(&bars)
                .0
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            ["6", "4", "2", "0"]
        );
        // 5 of a top of 6, 3 of 6, 0.
        assert!(bars.contains(r#"style="--size: 0.8333""#), "{bars}");
        assert!(bars.contains(r#"style="--size: 0.5000""#), "{bars}");
        assert!(bars.contains(r#"style="--size: 0.0000""#), "{bars}");
    }

    /// The note over a drawing follows how far the records go back, not
    /// which days are non-zero: the flat line before the first record is
    /// not data, and the note says so.
    #[test]
    fn the_history_note_follows_how_far_the_records_go_back() {
        let note = |values: &[i64], history: ChartHistory| {
            let m = card(&days(values), history).line().into_string();
            m.split(r#"<p class="chart__note">"#)
                .nth(1)
                .map(|rest| rest.split("</p>").next().unwrap().to_string())
        };
        let day =
            |d: u32| ChartHistory::Since(chrono::NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        // `days(..)` runs from Sep 1.
        assert_eq!(
            note(&[0, 0, 4], day(3)).as_deref(),
            Some("Collecting data — 1 day so far")
        );
        assert_eq!(
            note(&[0, 2, 4], day(2)).as_deref(),
            Some("Collecting data — 2 days so far")
        );
        // Covered, unknown, or an `Empty` history read before a record that
        // the series then saw: the drawing speaks for itself.
        assert_eq!(note(&[0, 0, 1], day(1)), None);
        assert_eq!(
            note(
                &[0, 0, 1],
                ChartHistory::Since(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
            ),
            None
        );
        assert_eq!(note(&[0, 0, 1], ChartHistory::Unknown), None);
        assert_eq!(note(&[0, 0, 1], ChartHistory::Empty), None);
    }

    /// A window that is zero every day is not drawn (layout rule 10): the
    /// card keeps its head and holds one `empty_note` sentence, which says
    /// whether the zero was measured. A flat line along the axis read as a
    /// chart with nothing in it, and four of them were a fresh install's
    /// whole dashboard.
    #[test]
    fn an_all_zero_window_is_a_sentence_not_a_drawing() {
        let day =
            |d: u32| ChartHistory::Since(chrono::NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        let cases = [
            (ChartHistory::Empty, "No new users recorded yet"),
            (day(3), "No new users yet — collecting data, 1 day so far"),
            (day(2), "No new users yet — collecting data, 2 days so far"),
            (day(1), "No new users in the last 3 days"),
            (
                ChartHistory::Since(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
                "No new users in the last 3 days",
            ),
            (ChartHistory::Unknown, "No new users in the last 3 days"),
        ];
        for (history, sentence) in cases {
            for m in both(&days(&[0, 0, 0]), history) {
                assert!(
                    m.contains(&format!(
                        r#"<div class="card__body"><p class="empty-note">{sentence}</p></div>"#
                    )),
                    "{history:?}: {m}"
                );
                for drawn in ["chart", "chart__note", "chart__range", "chart__values"] {
                    assert!(
                        !m.contains(&format!(r#"class="{drawn}""#)),
                        "{history:?}: an all-zero window draws no {drawn}: {m}"
                    );
                }
                assert!(
                    m.contains(r#"<a class="card__link" href="/b/admin/users">View users<svg"#)
                        && m.contains(r#"<p class="card__subtitle">Last 30 days</p>"#),
                    "the head is the drawn card's: {m}"
                );
                assert!(
                    crate::test_support::dom::card_level_inside_card(&m).is_empty(),
                    "{m}"
                );
            }
        }
    }

    #[test]
    fn value_axis_ticks_are_distinct_whole_numbers_up_to_a_top_covering_the_max() {
        use super::ValueAxis;
        let cases: &[(i64, &[i64])] = &[
            (0, &[0]),
            (1, &[1, 0]),
            (2, &[2, 1, 0]),
            (3, &[3, 2, 1, 0]),
            (4, &[4, 3, 2, 1, 0]),
            (5, &[6, 4, 2, 0]),
            (7, &[8, 6, 4, 2, 0]),
            (10, &[10, 5, 0]),
            (11, &[15, 10, 5, 0]),
            (37, &[40, 30, 20, 10, 0]),
            (100, &[100, 50, 0]),
            (1234, &[1500, 1000, 500, 0]),
        ];
        for (max, want) in cases {
            assert_eq!(
                ValueAxis::for_max(*max).ticks(),
                want.to_vec(),
                "axis for a max of {max}"
            );
        }
    }

    /// Every rendered y-axis label as `(text, --tick-y percentage)`, and every
    /// gridline's `y1` in viewBox units, in document order.
    fn axis_of(markup: &str) -> (Vec<(String, f64)>, Vec<f64>) {
        let labels = markup
            .split(r#"class="chart__ytick" style="--tick-y: "#)
            .skip(1)
            .map(|rest| {
                let (pct, rest) = rest.split_once("%\">").unwrap();
                let text = rest.split("</span>").next().unwrap();
                (text.to_string(), pct.parse::<f64>().unwrap())
            })
            .collect();
        let gridlines = markup
            .split(r#"class="chart__gridline" x1="0" x2="100" y1=""#)
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap().parse::<f64>().unwrap())
            .collect();
        (labels, gridlines)
    }

    /// A fresh install's New users series is 0 every day and 1 today. Its
    /// labels used to be spread evenly over four gridlines as "1", "0", "",
    /// "": the "0" sat on the gridline a third of the way down while the line
    /// plotted zero on the bottom edge. Each label must sit on the gridline
    /// for its own value, and the zero label on the baseline the zeros are
    /// drawn along.
    #[test]
    fn line_chart_labels_sit_on_the_gridline_for_their_value() {
        let data = vec![("2026-08-01".to_string(), 0), ("2026-08-02".to_string(), 1)];
        let m = card(&data, ChartHistory::Unknown).line().into_string();
        let (labels, gridlines) = axis_of(&m);
        assert_eq!(
            labels,
            vec![("1".to_string(), 0.0), ("0".to_string(), 100.0)],
            "{m}"
        );
        // Gridlines in the 60-unit viewBox, labels in percent of the same
        // height: the same positions.
        assert_eq!(gridlines, vec![0.0, 60.0], "{m}");
        assert!(
            m.contains(r#"points="0.00,60.00 100.00,0.00""#),
            "the zero is plotted on the baseline, the one at the top: {m}"
        );
    }
}

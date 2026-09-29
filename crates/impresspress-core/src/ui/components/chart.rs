//! Bar chart card. Moved from `blocks/admin/pages/dashboard.rs`.

use maud::{html, Markup};

/// Render a 30-day column bar chart card. `data` is ordered
/// chronologically; bars are normalized against the max count.
pub fn bar_chart_card(
    title: &str,
    subtitle: &str,
    data: &[(String, i64)],
    color_var: &str,
    view_href: &str,
) -> maud::Markup {
    let max = data.iter().map(|(_, v)| *v).max().unwrap_or(0).max(1);
    html! {
        section .card {
            header .card__head {
                div {
                    h2 .card__title { (title) }
                    p .card__subtitle { (subtitle) }
                }
                a .btn .btn--ghost .btn--sm .card__actions href=(view_href) { "View" }
            }
            div .card__body {
                table .charts-css .column style=(format!("--chart-color: {color_var}")) {
                    tbody {
                        @for (day, val) in data {
                            tr data-tooltip=(format!("{day}: {val}")) {
                                td style=(format!("--size: {:.4}", *val as f64 / max as f64)) {
                                    (val)
                                }
                            }
                        }
                    }
                }
                (date_range(data))
            }
        }
    }
}

/// The first / last date labels under a 30-day chart.
///
/// Each label is a `<time>` carrying the ISO day, because it is a date: the
/// window ends today, so the text changes every day, and the visual-baseline
/// suite masks `time` elements for exactly that reason.
fn date_range(data: &[(String, i64)]) -> Markup {
    let label = |day: Option<&String>| -> Markup {
        match day {
            Some(day) => {
                let short = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                    .map(|d| d.format("%b %-d").to_string())
                    .unwrap_or_else(|_| day.clone());
                html! { time datetime=(day) { (short) } }
            }
            None => html! { span {} },
        }
    };
    html! {
        div .charts-css__range {
            (label(data.first().map(|(d, _)| d)))
            (label(data.last().map(|(d, _)| d)))
        }
    }
}

/// Tiny inline-SVG trend line for a stat tile. Decorative: the tile already
/// states the value in text, so this is aria-hidden.
///
/// Uses a 100x24 viewBox with `preserveAspectRatio="none"` so it stretches to
/// whatever width the tile gives it without needing a layout measurement.
pub fn sparkline(series: &[i64], color_var: &str) -> Markup {
    if series.is_empty() {
        return html! {};
    }
    let max = series.iter().copied().max().unwrap_or(0);
    let min = series.iter().copied().min().unwrap_or(0);
    // A flat series has zero span. Rendering it along the bottom edge would
    // read as "zero" / "lowest", which is wrong when the value is nonzero —
    // a flat line means "no change", so it belongs at the vertical centre
    // of the 24-tall viewBox (y = 12.0), not at y = 24.0.
    let flat = max == min;
    let step = if series.len() > 1 {
        100.0 / (series.len() - 1) as f64
    } else {
        0.0
    };
    let points = series
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = i as f64 * step;
            let y = if flat {
                12.0
            } else {
                24.0 - ((*v - min) as f64 / (max - min) as f64) * 24.0
            };
            format!("{x:.2},{y:.2}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    html! {
        svg .sparkline viewBox="0 0 100 24" preserveAspectRatio="none"
            aria-hidden="true" style=(format!("--chart-color: {color_var}")) {
            // See the note in `line_chart_card` on why SVG shape elements use `{}`
            // rather than maud's `;` void syntax — it matters once a shape has
            // siblings, but keeping it consistent here avoids relying on the
            // enclosing `</svg>` to implicitly close a dangling tag.
            //
            // `vector-effect="non-scaling-stroke"` matters here too: `.stat-spark`
            // is 4rem x 1.5rem, a different aspect ratio than the 100x24 viewBox,
            // so preserveAspectRatio="none" scales x and y unevenly. Without this,
            // the stroke renders visibly thicker in one axis than the other.
            polyline points=(points) fill="none" stroke="var(--chart-color)"
                stroke-width="1.5" vector-effect="non-scaling-stroke" {}
        }
    }
}

/// The value axis of a [`line_chart_card`]: integer ticks from 0 to `top`,
/// `step` apart, where every tick sits on its own gridline.
#[derive(Debug, PartialEq, Eq)]
struct ValueAxis {
    /// The value at the top of the plot. 0 only for a series that is zero
    /// throughout, which is then plotted along the baseline.
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
    /// data it has no reason to show: a series that is zero throughout gets the
    /// single tick 0, not a "1" the data never reaches.
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

/// 30-day line + area chart with gridlines and y-axis ticks.
///
/// `bar_chart_card` renders the same data as columns; pick per series —
/// the dashboard uses bars for Requests and lines for New users / Errors.
///
/// The plot, its gridlines, its y-axis labels and its endpoint dot are all
/// placed from one [`ValueAxis`]: a gridline and its label share the same
/// fraction of the plot's height, so a label cannot sit beside another
/// tick's line.
pub fn line_chart_card(
    title: &str,
    subtitle: &str,
    data: &[(String, i64)],
    color_var: &str,
    view_href: &str,
) -> Markup {
    let axis = ValueAxis::for_max(data.iter().map(|(_, v)| *v).max().unwrap_or(0));
    // Distance from the top of the plot as a percentage, the unit the labels
    // and the dot are positioned in; the SVG uses the same value scaled to its
    // 60-unit-tall viewBox.
    let from_top = |value: i64| (1.0 - axis.fraction(value)) * 100.0;
    let step = if data.len() > 1 {
        100.0 / (data.len() - 1) as f64
    } else {
        0.0
    };
    let line = data
        .iter()
        .enumerate()
        .map(|(i, (_, v))| format!("{:.2},{:.2}", i as f64 * step, from_top(*v) * 0.6))
        .collect::<Vec<_>>()
        .join(" ");
    let area = format!("0,60 {line} 100,60");
    let ticks = axis.ticks();
    html! {
        section .card {
            header .card__head {
                div {
                    h2 .card__title { (title) }
                    p .card__subtitle { (subtitle) }
                }
                a .btn .btn--ghost .btn--sm .card__actions href=(view_href) { "View" }
            }
            div .card__body {
                div .chart {
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
                    // itself) so it's visible to both the plot and the `.chart__dot`
                    // div below — a CSS custom property only inherits to descendants
                    // of the element it's set on, and the dot is now a sibling of the
                    // svg, not nested inside it.
                    div .chart__plot-wrap style=(format!("--chart-color: {color_var}")) {
                        svg .chart__plot viewBox="0 0 100 60" preserveAspectRatio="none"
                            role="img" aria-label=(format!("{title}, {subtitle}")) {
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
                            polygon .chart__area points=(area) {}
                            polyline .chart__line points=(line) fill="none" {}
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
                        @if let Some((_, last)) = data.last() {
                            div .chart__dot style=(format!("--dot-y: {:.2}%", from_top(*last))) {}
                        }
                    }
                }
                (date_range(data))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sparkline_emits_one_point_per_sample_and_is_decorative() {
        let m = super::sparkline(&[0, 5, 3], "var(--primary-color)").into_string();
        assert!(m.contains("<svg"));
        assert!(
            m.contains(r#"aria-hidden="true""#),
            "sparkline duplicates the value beside it"
        );
        let pts = m
            .split("points=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert_eq!(
            pts.split_whitespace().count(),
            3,
            "one point per sample: {pts}"
        );
    }

    #[test]
    fn sparkline_flat_series_does_not_divide_by_zero() {
        let m = super::sparkline(&[4, 4, 4], "var(--primary-color)").into_string();
        assert!(m.contains("points="), "flat series must still render");
        assert!(!m.contains("NaN"), "flat series produced NaN: {m}");
    }

    #[test]
    fn sparkline_flat_series_renders_at_vertical_centre() {
        // A flat line means "no change"; drawing it along the bottom edge
        // (y = 24.0) would misleadingly read as "zero" / "lowest" instead.
        // It must sit at the viewBox's vertical midpoint, y = 12.0.
        let m = super::sparkline(&[4, 4, 4], "var(--primary-color)").into_string();
        let pts = m
            .split("points=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        for point in pts.split_whitespace() {
            let y = point.split(',').nth(1).unwrap();
            assert_eq!(
                y, "12.00",
                "flat series must sit at the centre, not an edge: {pts}"
            );
        }
    }

    #[test]
    fn sparkline_empty_series_renders_nothing() {
        assert_eq!(
            super::sparkline(&[], "var(--primary-color)").into_string(),
            ""
        );
    }

    #[test]
    fn line_chart_card_has_gridlines_and_axis_labels() {
        let data = vec![("2026-08-01".to_string(), 1), ("2026-08-02".to_string(), 3)];
        let m = super::line_chart_card(
            "New users",
            "Last 30 days",
            &data,
            "var(--primary-color)",
            "/b/admin/users",
        )
        .into_string();
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
        let cards = [
            super::line_chart_card("Errors", "Last 30 days", &data, "var(--x)", "/x"),
            super::bar_chart_card("Requests", "Last 30 days", &data, "var(--x)", "/x"),
        ];
        for card in cards {
            let m = card.into_string();
            assert!(
                m.contains(r#"<time datetime="2026-08-27">Aug 27</time>"#)
                    && m.contains(r#"<time datetime="2026-09-25">Sep 25</time>"#),
                "range labels are not <time> elements: {m}"
            );
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
        let m = super::line_chart_card("New users", "Last 30 days", &data, "var(--x)", "/x")
            .into_string();
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

    /// The Errors series on a healthy deployment is zero every day. Its axis
    /// labelled a "1" the data never reaches, because the scale was clamped to
    /// at least 1; it shows the one value there is.
    #[test]
    fn line_chart_all_zero_series_labels_only_zero() {
        let data = vec![("2026-08-01".to_string(), 0), ("2026-08-02".to_string(), 0)];
        let m =
            super::line_chart_card("Errors", "Last 30 days", &data, "var(--x)", "/x").into_string();
        let (labels, gridlines) = axis_of(&m);
        assert_eq!(labels, vec![("0".to_string(), 100.0)], "{m}");
        assert_eq!(gridlines, vec![60.0], "{m}");
        assert!(
            m.contains(r#"points="0.00,60.00 100.00,60.00""#),
            "an all-zero series is drawn along the baseline: {m}"
        );
        assert!(
            m.contains("--dot-y: 100.00%"),
            "the endpoint dot sits on the baseline: {m}"
        );
    }
}

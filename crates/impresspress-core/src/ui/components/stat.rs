//! Stat Card

use maud::{html, Markup};

/// Dashboard stat tile: icon, uppercase label, value, optional sparkline.
pub fn stat_card(label: &str, value: &str, icon: Markup, spark: Option<Markup>) -> Markup {
    StatCard::new(label, value, icon).spark(spark).render()
}

/// A [`stat_card`] with the two things only some tiles need: a line under
/// the value that breaks the figure down, and an alert state.
pub struct StatCard<'a> {
    label: &'a str,
    value: &'a str,
    icon: Markup,
    spark: Option<Markup>,
    detail: Option<Markup>,
    alert: bool,
}

impl<'a> StatCard<'a> {
    pub fn new(label: &'a str, value: &'a str, icon: Markup) -> Self {
        Self {
            label,
            value,
            icon,
            spark: None,
            detail: None,
            alert: false,
        }
    }

    /// The sparkline beside the icon, when the figure has a daily series.
    pub fn spark(mut self, spark: Option<Markup>) -> Self {
        self.spark = spark;
        self
    }

    /// One line under the value that breaks it down ("12 client errors").
    pub fn detail(mut self, detail: Option<Markup>) -> Self {
        self.detail = detail;
        self
    }

    /// Draw the tile in the danger tone: for a figure that is a problem
    /// whenever it is not zero. The label already says what is counted, so
    /// the colour is never the only signal.
    pub fn alert(mut self, alert: bool) -> Self {
        self.alert = alert;
        self
    }

    pub fn render(self) -> Markup {
        html! {
            div class=(if self.alert { "stat-card stat-card--alert" } else { "stat-card" }) {
                div .stat-header {
                    div .stat-icon { (self.icon) }
                    @if let Some(s) = self.spark { div .stat-spark { (s) } }
                }
                div .stat-label { (self.label) }
                div .stat-value { (self.value) }
                @if let Some(detail) = self.detail { div .stat-description { (detail) } }
            }
        }
    }
}

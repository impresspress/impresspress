use std::collections::HashMap;

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream, WaferError};

use super::{admin_page, crumb, status_code_badge_variant};
use crate::{
    blocks::auth::repo::users::{self, DailySignups},
    platform_state::request_logs::{self, DailyCounts, TodayCounts},
    ui::{
        components::{self, empty_note, AvatarSize, Badge, Card, ChartCard, DataTable, TableRow},
        shell::Topbar,
        templates::{dashboard_page, DashboardRow},
    },
};

/// What a stat shows when the aggregate behind it could not be read.
///
/// The dashboard is the page an operator opens *during* an outage, so a
/// failed read marks its own stat rather than failing the whole page (the
/// one exception to the group-1 rule). It must still be a marker and not a
/// number: `0` is a claim, and "Total users 0" during a database outage is
/// the claim that the deployment has no users.
const STAT_UNAVAILABLE: &str = "Unavailable";

/// The same marker for the card- and chart-shaped panels, which have a body
/// rather than a value. It is the card's one sentence, an `empty_note`.
const CARD_UNAVAILABLE: &str = "Could not load";

/// A stat's pre-formatted figure, or [`STAT_UNAVAILABLE`] when the aggregate
/// behind it could not be read.
fn stat_value(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or(STAT_UNAVAILABLE)
}

/// A chart card whose series could not be read: the same head, and the marker
/// where the plot would be.
///
/// It cannot reuse [`ChartCard`] with an empty series: that says "No … in the
/// last 30 days", a claim about a log nobody could read.
fn chart_unavailable_card(title: &str, link: Destination) -> Markup {
    Card::titled(title, empty_note(CARD_UNAVAILABLE, None))
        .subtitle(CHART_WINDOW)
        .link(link.label, link.href)
        .render()
}

/// Every chart's subtitle: the window its series covers.
const CHART_WINDOW: &str = "Last 30 days";

/// Where a card's head link goes, and the name it goes by.
///
/// A link's label is its accessible name, and a screen reader's links list
/// shows it out of context. So each destination has one label, and no two
/// destinations share one: the page's six head links read "View users" (×2),
/// "View logs", "View server errors" (×2) and "View client errors", where
/// they were four "View" and two "View all".
#[derive(Clone, Copy)]
struct Destination {
    label: &'static str,
    href: &'static str,
}

const USERS: Destination = Destination {
    label: "View users",
    href: "/b/admin/users",
};

const LOGS: Destination = Destination {
    label: "View logs",
    href: "/b/admin/logs",
};

/// The logs page narrowed by the same rule the server-error card and chart
/// count by (`request_logs::StatusClass`).
const SERVER_ERRORS_LOGS: Destination = Destination {
    label: "View server errors",
    href: "/b/admin/logs?server_errors=1",
};

const CLIENT_ERRORS_LOGS: Destination = Destination {
    label: "View client errors",
    href: "/b/admin/logs?client_errors=1",
};

/// Trailing 30-day window as `(oldest_day, oldest_day_midnight_iso)`.
/// `oldest_day` anchors the zero-fill; the ISO string is the `created_at >=`
/// lower bound shared by every 30-day query.
fn window_30d() -> (chrono::NaiveDate, String) {
    let today = chrono::Utc::now().date_naive();
    let start = today - chrono::Duration::days(29);
    (start, format!("{start}T00:00:00"))
}

/// Zero-fill `by_day` into a 30-entry series ordered oldest → newest
/// (matching the chart's x-axis). A missing day reads as `0`.
fn zero_filled_30d(by_day: &HashMap<String, i64>, start: chrono::NaiveDate) -> Vec<(String, i64)> {
    (0..30)
        .map(|i| {
            let date = (start + chrono::Duration::days(i))
                .format("%Y-%m-%d")
                .to_string();
            let count = by_day.get(&date).copied().unwrap_or(0);
            (date, count)
        })
        .collect()
}

/// Project the daily signup counts into a zero-filled 30-entry series.
fn series_from_signups(rows: &[DailySignups], start: chrono::NaiveDate) -> Vec<(String, i64)> {
    let by_day: HashMap<String, i64> = rows.iter().map(|r| (r.day.clone(), r.count)).collect();
    zero_filled_30d(&by_day, start)
}

/// Project one metric out of the request log's daily counts into a
/// zero-filled 30-entry series.
fn series_from_daily(
    days: &[DailyCounts],
    pick: fn(&DailyCounts) -> i64,
    start: chrono::NaiveDate,
) -> Vec<(String, i64)> {
    let by_day: HashMap<String, i64> = days.iter().map(|d| (d.day.clone(), pick(d))).collect();
    zero_filled_30d(&by_day, start)
}

pub async fn dashboard(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let today_start = format!("{today}T00:00:00");

    // Every read below is independent, so we issue them concurrently with
    // `futures::join!`. Concurrency alone doesn't cut the D1 *statement* count,
    // though — each round-trip is a billed statement on Cloudflare — so the
    // header figures now fold their several per-filter counts into ONE aggregate
    // per table (conditional `CaseWhenSum` columns), and the two REQUEST_LOGS
    // chart series come from ONE grouped-by-day statement. That is eight D1
    // statements to render the whole page (was ten): two consolidated header
    // aggregates, two recent-row lists, two daily grouped aggregates, and two
    // one-row reads of the oldest account and the oldest logged request,
    // which tell each chart how far back its series really goes.
    let (start_30d, start_iso) = window_30d();

    let user_counts_fut = users::active_count_and_created_since(ctx, &today_start);
    let request_counts_fut = request_logs::today_counts(ctx, &today_start);

    let users_daily_fut = users::daily_signups(ctx, &start_iso);
    let requests_daily_fut = request_logs::daily_counts(ctx, &start_iso);

    let recent_users_fut = users::list_recent_active(ctx, 5);

    let recent_errors_fut = request_logs::list_recent_server_errors(ctx, 5);

    let first_user_fut = users::first_created_at(ctx);
    let first_request_fut = request_logs::first_logged_at(ctx);

    let (
        user_counts_r,
        request_counts_r,
        recent_users_r,
        recent_errors_r,
        users_daily_r,
        request_days_r,
        first_user_r,
        first_request_r,
    ) = futures::join!(
        user_counts_fut,
        request_counts_fut,
        recent_users_fut,
        recent_errors_fut,
        users_daily_fut,
        requests_daily_fut,
        first_user_fut,
        first_request_fut,
    );

    // How far back each chart's records go. A failed read is `Unknown`,
    // which makes no claim about it: a series with data is drawn without the
    // history note, and an all-zero one says only that nothing was recorded
    // in the window (`ChartCard`).
    let history = |first: Result<Option<String>, WaferError>, what: &str| match first {
        Ok(Some(at)) => at
            .get(..10)
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .map_or(
                components::ChartHistory::Unknown,
                components::ChartHistory::Since,
            ),
        Ok(None) => components::ChartHistory::Empty,
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: oldest {what} read failed");
            components::ChartHistory::Unknown
        }
    };
    let users_history = history(first_user_r, "account");
    let requests_history = history(first_request_r, "request log");

    // Each read is logged and marked where it renders, rather than being
    // published as `0` and the empty state. Everything below reads `Option`:
    // `None` is "could not be read", and it never reaches a figure, a table
    // row or a chart.
    let request_counts = match request_counts_r {
        Ok(counts) => Some(counts),
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: today's request counts failed");
            None
        }
    };
    let user_counts = match user_counts_r {
        Ok(counts) => Some(counts),
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: user counts failed");
            None
        }
    };
    let recent_users = match recent_users_r {
        Ok(rows) => Some(rows),
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: recent users failed");
            None
        }
    };
    let recent_errors = match recent_errors_r {
        Ok(rows) => Some(rows),
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: recent errors failed");
            None
        }
    };
    let users_daily_rows = match users_daily_r {
        Ok(rows) => Some(rows),
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: daily signups failed");
            None
        }
    };
    let request_days = match request_days_r {
        Ok(rows) => Some(rows),
        Err(e) => {
            tracing::error!(error = %e, "admin dashboard: daily request counts failed");
            None
        }
    };

    // Two grouped statements back all four charts: the USERS series comes from
    // its own daily aggregate; the request-log "requests", "client errors" and
    // "server errors" series are three metrics projected out of the *same*
    // per-day counts.
    //
    // An unreadable aggregate must NOT be zero-filled: the 30-day fill turns
    // "we could not read this" into a flat line along the axis, which is a
    // picture of "nothing happened".
    let new_users_daily = users_daily_rows
        .as_ref()
        .map(|rows| series_from_signups(rows, start_30d));
    let requests_daily = request_days
        .as_ref()
        .map(|days| series_from_daily(days, |d| d.requests, start_30d));
    let client_errors_daily = request_days
        .as_ref()
        .map(|days| series_from_daily(days, |d| d.client_errors, start_30d));
    let server_errors_daily = request_days
        .as_ref()
        .map(|days| series_from_daily(days, |d| d.server_errors, start_30d));

    let user_count_str = user_counts.map(|(total, _)| total.to_string());
    let new_users_str = user_counts.map(|(_, today)| today.to_string());
    let requests_str = request_counts
        .as_ref()
        .map(|c: &TodayCounts| c.requests.to_string());
    let server_errors = request_counts
        .as_ref()
        .map(|c: &TodayCounts| c.server_errors);
    let server_errors_str = server_errors.map(|n| n.to_string());
    // The client errors are a breakdown of the request figure, not a figure
    // of their own: mostly mistyped or probing URLs, missing credentials and
    // rate limits, which say nothing about this deployment's health.
    let requests_detail = request_counts.as_ref().map(|c: &TodayCounts| {
        html! {
            (plural(c.client_errors, "client error", "client errors"))
            @if c.rate_limited > 0 {
                " · " (c.rate_limited) " rate-limited"
            }
        }
    });
    let avg_ms_str = request_counts
        .as_ref()
        .map(|c: &TodayCounts| format!("{:.0}ms", c.avg_ms));

    // Five figures, no trend lines: the charts below draw the 30-day series.
    let stats = vec![
        components::Stat::new("Total users", stat_value(&user_count_str)),
        components::Stat::new("New today", stat_value(&new_users_str)),
        components::Stat::new("Requests today", stat_value(&requests_str)).detail(requests_detail),
        // Server errors (5xx) only: this deployment failing. Red whenever it
        // is not zero, and the "Recent server errors" card and the logs
        // page's "Server errors" filter it links to count the same rows
        // (`request_logs::StatusClass`).
        components::Stat::new("Server errors today", stat_value(&server_errors_str))
            .alert(server_errors.is_some_and(|n| n > 0)),
        components::Stat::new("Avg response", stat_value(&avg_ms_str)),
    ];

    // Both recent-rows cards are flush whatever they hold: the table's frame
    // is the card's (layout rule 2), and `DataTable::empty_note` and a bare
    // `empty_note` take the card's inset back, so the sentence lines up with
    // the title (card.css).
    let recent_users_card = {
        let body = match &recent_users {
            Some(recent_users) => {
                let rows: Vec<TableRow> = recent_users
                    .iter()
                    .map(|record| {
                        let created = record.created_at.as_str();
                        TableRow::new(vec![
                            html! {
                                span .recent-user {
                                    (components::avatar(&record.email, AvatarSize::Md))
                                    span { (components::breakable_id(&record.email)) }
                                }
                            },
                            // `.text-right` needs a block box to align against,
                            // and the component owns the `<td>`.
                            html! { div .text-muted .text-right { (components::timestamp(created)) } },
                        ])
                    })
                    .collect();
                DataTable::new(&RECENT_USERS_COLUMNS)
                    .rows(rows)
                    .headless()
                    .empty_note(NO_USERS_YET)
                    .render()
            }
            None => empty_note(CARD_UNAVAILABLE, None),
        };
        Card::titled("Recent users", body)
            .link(USERS.label, USERS.href)
            .flush()
            .render()
    };

    let recent_errors_card = {
        let body = match &recent_errors {
            Some(recent_errors) => {
                let rows: Vec<TableRow> = recent_errors
                    .iter()
                    .map(|row| {
                        let code = row.status_code;
                        let created = row.created_at.as_str();
                        TableRow::new(vec![
                            Badge::new(status_code_badge_variant(code)).render(html! { (code) }),
                            html! { span .font-medium { (row.method.to_uppercase()) } },
                            html! { (components::breakable_id(&row.path)) },
                            html! { span .text-muted { (components::timestamp(created)) } },
                        ])
                    })
                    .collect();
                DataTable::new(&RECENT_ERRORS_COLUMNS)
                    .rows(rows)
                    .empty_note(NO_RECENT_SERVER_ERRORS)
                    .render()
            }
            None => empty_note(CARD_UNAVAILABLE, None),
        };
        Card::titled("Recent server errors", body)
            .link(SERVER_ERRORS_LOGS.label, SERVER_ERRORS_LOGS.href)
            .flush()
            .render()
    };

    // Each chart is drawn when its window has data; a window that is zero
    // every day is one sentence in the card (`ChartCard`, layout rule 10).
    let new_users_chart = ChartPanel {
        title: "New users",
        counts: "new users",
        history: users_history,
        color_var: "var(--primary-color)",
        link: USERS,
        draw: |card| card.line(),
    }
    .render(new_users_daily.as_deref());
    let requests_chart = ChartPanel {
        title: "Requests",
        counts: "requests",
        history: requests_history,
        color_var: NEUTRAL_SERIES,
        link: LOGS,
        draw: |card| card.bars(),
    }
    .render(requests_daily.as_deref());
    // Drawn only when the window holds a server error, so always in the
    // danger red; a quiet window is the sentence, not a grey line.
    let server_errors_chart = ChartPanel {
        title: "Server errors",
        counts: "server errors",
        history: requests_history,
        color_var: "var(--accent-danger)",
        link: SERVER_ERRORS_LOGS,
        draw: |card| card.line(),
    }
    .render(server_errors_daily.as_deref());
    let client_errors_chart = ChartPanel {
        title: "Client errors",
        counts: "client errors",
        history: requests_history,
        color_var: QUIET_SERIES,
        link: CLIENT_ERRORS_LOGS,
        draw: |card| card.line(),
    }
    .render(client_errors_daily.as_deref());

    let body = dashboard_page(
        stats,
        vec![
            DashboardRow::charts(html! { (new_users_chart) (requests_chart) }),
            DashboardRow::charts(html! { (server_errors_chart) (client_errors_chart) }),
            DashboardRow::tables(html! { (recent_users_card) (recent_errors_card) }),
        ],
    );

    admin_page(
        ctx,
        msg,
        "Dashboard",
        Topbar {
            crumbs: crumb("Dashboard"),
            subtitle: Some("System overview"),
            ..Topbar::default()
        },
        body,
    )
    .await
}

/// One chart panel: everything but its series, by name, so no two of its
/// strings can trade places at a call site.
struct ChartPanel {
    title: &'static str,
    /// What the series counts, in running text: [`ChartCard::counts`].
    counts: &'static str,
    history: components::ChartHistory,
    color_var: &'static str,
    link: Destination,
    /// [`ChartCard::line`] or [`ChartCard::bars`].
    draw: fn(ChartCard<'_>) -> Markup,
}

impl ChartPanel {
    /// The series drawn (or its all-zero sentence), or, when the series could
    /// not be read (`None`), the unavailable marker under the same head.
    fn render(self, series: Option<&[(String, i64)]>) -> Markup {
        match series {
            Some(data) => (self.draw)(ChartCard {
                title: self.title,
                subtitle: CHART_WINDOW,
                counts: self.counts,
                data,
                history: self.history,
                color_var: self.color_var,
                link: (self.link.label, self.link.href),
            }),
            None => chart_unavailable_card(self.title, self.link),
        }
    }
}

/// The colour of a series that is a count of traffic: neutral ink.
const NEUTRAL_SERIES: &str = "var(--text-primary)";

/// The colour of a secondary series.
const QUIET_SERIES: &str = "var(--text-secondary)";

/// The "Recent users" card's empty note.
const NO_USERS_YET: &str = "No users yet";

/// The "Recent server errors" card's empty note.
const NO_RECENT_SERVER_ERRORS: &str = "No server errors recently";

/// `"1 client error"` / `"3 client errors"`.
fn plural(n: i64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The dashboard cards' table columns. "Recent users" renders headless — it
/// reads as a two-column list, not a grid — but still declares its columns,
/// because the `data-label` the component stamps on every `<td>` is what names
/// the cells when the table collapses to cards on a narrow viewport. It has no
/// status column: the card lists active accounts only
/// (`users::list_recent_active`), and a state badge shows only a non-default
/// state (layout rule 11), which no row here has.
const RECENT_USERS_COLUMNS: [components::TableCol<'static>; 2] = [
    components::TableCol::new("Email").primary(),
    components::TableCol::new("Created"),
];

const RECENT_ERRORS_COLUMNS: [components::TableCol<'static>; 4] = [
    components::TableCol::new("Status"),
    components::TableCol::new("Method"),
    components::TableCol::new("Path").primary(),
    components::TableCol::new("Time"),
];

#[cfg(test)]
mod tests {
    //! The dashboard's own arithmetic: the 30-day zero-fill that turns a
    //! sparse per-day aggregate into the chart's x-axis. The aggregates
    //! themselves are `auth::repo::users` and `platform_state::request_logs`
    //! functions now, and are checked against separate `db::count` calls
    //! beside their owners.

    use std::collections::HashMap;

    use super::{series_from_signups, window_30d, zero_filled_30d};
    use crate::blocks::auth::repo::users::DailySignups;

    /// Value for `date` in a `(date, count)` series, or `-1` if absent.
    fn day_value(series: &[(String, i64)], date: &str) -> i64 {
        series
            .iter()
            .find(|(d, _)| d == date)
            .map(|(_, c)| *c)
            .unwrap_or(-1)
    }

    #[test]
    fn signup_series_is_zero_filled_over_the_thirty_day_window() {
        let (start_30d, _) = window_30d();
        let day = |ago: i64| {
            (chrono::Utc::now().date_naive() - chrono::Duration::days(ago))
                .format("%Y-%m-%d")
                .to_string()
        };
        let rows = vec![
            DailySignups {
                day: day(0),
                count: 3,
            },
            DailySignups {
                day: day(5),
                count: 2,
            },
            // Outside the window: dropped rather than folded into an edge day.
            DailySignups {
                day: day(40),
                count: 9,
            },
        ];
        let series = series_from_signups(&rows, start_30d);
        assert_eq!(series.len(), 30, "30-entry zero-filled series");
        assert_eq!(day_value(&series, &day(0)), 3);
        assert_eq!(day_value(&series, &day(5)), 2);
        assert_eq!(day_value(&series, &day(1)), 0, "a quiet day reads as zero");
        assert_eq!(
            series.iter().map(|(_, c)| c).sum::<i64>(),
            5,
            "the 40-days-ago row is outside the window"
        );
    }

    #[test]
    fn zero_fill_starts_at_the_window_start_and_runs_forward() {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let series = zero_filled_30d(&HashMap::from([("2026-01-03".to_string(), 7)]), start);
        assert_eq!(series[0].0, "2026-01-01");
        assert_eq!(series[29].0, "2026-01-30");
        assert_eq!(series[2], ("2026-01-03".to_string(), 7));
    }
}

#[cfg(test)]
mod outage_tests {
    //! The dashboard is the one page in group 1 that must NOT fail whole.
    //!
    //! It aggregates six independent reads and it is the page an operator
    //! opens *during* an outage, so a failed read marks its own stat and the
    //! rest of the page still renders. What it must never do is what it did:
    //! publish the failure as the number `0` and the empty state, so a
    //! deployment in trouble rendered as a healthy, unused one — "Total Users
    //! 0", "Errors Today 0", "No server errors recently", and flat charts
    //! along the axis.

    use super::*;
    use crate::test_support::{admin_msg, output_html, output_http_status, TestContext};

    #[tokio::test]
    async fn every_failed_read_marks_its_own_stat_and_the_page_still_renders() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let msg = admin_msg("retrieve", "/b/admin/");

        assert_eq!(
            output_http_status(dashboard(&ctx, &msg).await).await,
            200,
            "the dashboard must survive an outage — it is what an operator opens during one"
        );

        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;

        assert!(
            !html.contains(r#"<div class="stat__value">0</div>"#),
            "a failed read must not be published as the figure 0: {html}"
        );
        assert_eq!(
            html.matches(STAT_UNAVAILABLE).count(),
            5,
            "all five stats are fed by the two failed aggregates"
        );
        assert!(
            !html.contains(NO_RECENT_SERVER_ERRORS),
            "an unreadable error log must not render as 'no errors recently': {html}"
        );
        assert!(
            !html.contains("stat--alert"),
            "an unreadable server-error count is not an alert: {html}"
        );
        assert!(
            !html.contains("No users yet"),
            "an unreadable user list must not render as 'no users yet': {html}"
        );
        assert_eq!(
            html.matches(&format!(r#"<p class="empty-note">{CARD_UNAVAILABLE}</p>"#))
                .count(),
            6,
            "two recent-row cards and four chart cards each carry a marker: {html}"
        );
        assert!(
            !html.contains("in the last 30 days") && !html.contains("recorded yet"),
            "an unreadable series must not be published as a quiet window: {html}"
        );
        assert!(
            !html.contains("chart__plot") && !html.contains("chart__bar"),
            "a chart whose series could not be read must not be plotted at all — a \
             zero-filled 30-day series draws a flat line along the axis, which is a \
             picture of 'nothing happened': {html}"
        );
    }

    /// The healthy render of a fresh install: real figures and the genuine
    /// empty notes, with no marker anywhere. Every series is zero, so no
    /// chart is drawn: each chart card holds one sentence.
    #[tokio::test]
    async fn a_healthy_dashboard_carries_no_marker() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;

        assert!(
            !html.contains(STAT_UNAVAILABLE) && !html.contains(CARD_UNAVAILABLE),
            "a healthy dashboard carries no unavailable marker: {html}"
        );
        assert!(
            html.contains("No users yet") && html.contains(NO_RECENT_SERVER_ERRORS),
            "the genuine empty states still render on a healthy, unused deployment: {html}"
        );
        assert_eq!(
            html.matches(r#"class="card__subtitle">Last 30 days<"#)
                .count(),
            4,
            "all four chart cards render"
        );
        assert!(
            !html.contains(r#"class="chart""#),
            "a fresh install's windows are all zero, so nothing is drawn: {html}"
        );
        assert_eq!(
            html.matches(r#"<p class="empty-note">"#).count(),
            6,
            "four chart sentences and the two recent-rows notes: {html}"
        );
    }

    /// A window with data is drawn, with its "Collecting data" note while the
    /// records are younger than the window; a quiet window beside it is one
    /// sentence. This is the visual baseline's state: one admin and some
    /// traffic today, no error.
    #[tokio::test]
    async fn a_window_with_data_is_drawn_and_a_quiet_one_is_a_sentence() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        // Created now, inside the window (`seed_auth_user` dates its row
        // 2026-01-01).
        users::insert(
            &ctx.fixture(),
            users::NewUser {
                email: "today@example.com".into(),
                display_name: "Today".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed a user created today");
        request_logs::insert(
            &ctx,
            &request_logs::NewRequestLog {
                method: "GET",
                path: "/served",
                status_code: 200,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
        )
        .await
        .expect("seed a request log row");
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        let doc = scraper::Html::parse_document(&html);
        let selector = |s: &str| scraper::Selector::parse(s).unwrap();
        let card_body = |title: &str| {
            doc.select(&selector("section.card"))
                .find(|card| {
                    card.select(&selector("h2.card__title"))
                        .any(|h| h.text().collect::<String>() == title)
                })
                .unwrap_or_else(|| panic!("no {title} card: {html}"))
                .select(&selector(".card__body"))
                .next()
                .unwrap()
                .inner_html()
        };

        let users = card_body("New users");
        assert!(
            users.contains("chart__plot") && users.contains("chart__line"),
            "{users}"
        );
        let requests = card_body("Requests");
        assert!(requests.contains("chart__bar"), "{requests}");
        for drawn in [&users, &requests] {
            assert!(
                drawn.starts_with(r#"<p class="chart__note">Collecting data — 1 day so far</p>"#),
                "a series with data keeps its chart and its note: {drawn}"
            );
            assert!(!drawn.contains("empty-note"), "{drawn}");
        }
        assert_eq!(
            card_body("Server errors"),
            r#"<p class="empty-note">No server errors yet — collecting data, 1 day so far</p>"#
        );
        assert_eq!(
            card_body("Client errors"),
            r#"<p class="empty-note">No client errors yet — collecting data, 1 day so far</p>"#
        );
    }

    /// Five figures in one row, labelled in sentence case as written, with
    /// nothing drawn: the 30-day series are the charts' to draw. The error
    /// figure says what it counts.
    #[tokio::test]
    async fn the_stat_row_is_five_sentence_case_figures_with_no_drawing() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        let start = html
            .find(r#"<div class="stat-row">"#)
            .expect("the stat row");
        let row = &html[start..html[start..].find("dashboard-row").unwrap() + start];
        let labels: Vec<&str> = row
            .split(r#"<div class="stat__label">"#)
            .skip(1)
            .map(|rest| &rest[..rest.find('<').unwrap()])
            .collect();
        assert_eq!(
            labels,
            [
                "Total users",
                "New today",
                "Requests today",
                "Server errors today",
                "Avg response"
            ],
            "{row}"
        );
        assert!(
            !row.contains("<svg"),
            "a stat draws no icon and no trend: {row}"
        );
        assert!(!html.contains("4xx/5xx"), "{html}");
    }

    /// The markup of the stat labelled `label`, from its opening `.stat` to
    /// the end of its value and detail.
    fn stat(html: &str, label: &str) -> String {
        const OPEN: [&str; 2] = [r#"<div class="stat">"#, r#"<div class="stat stat--alert">"#];
        let at = html
            .find(&format!(r#"<div class="stat__label">{label}</div>"#))
            .unwrap_or_else(|| panic!("no {label} stat: {html}"));
        let start = OPEN
            .iter()
            .filter_map(|open| html[..at].rfind(open))
            .max()
            .unwrap();
        let end = OPEN
            .iter()
            .filter_map(|open| html[at..].find(open))
            .min()
            .map_or(html.len(), |n| at + n);
        html[start..end].to_string()
    }

    /// One request of each class today: the 500 is the headline figure, red,
    /// and the one row in the "Recent server errors" card; the 404 and the
    /// 429 are a neutral breakdown under the request count, and never reach
    /// the card.
    #[tokio::test]
    async fn server_errors_headline_and_client_errors_break_down_the_requests() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for (path, status_code) in [
            ("/served", 200),
            ("/refused-missing", 404),
            ("/refused-throttled", 429),
            ("/failed", 500),
        ] {
            request_logs::insert(
                &ctx,
                &request_logs::NewRequestLog {
                    method: "GET",
                    path,
                    status_code,
                    error_message: "",
                    duration_ms: 1,
                    client_ip: "203.0.113.7",
                    user_id: "",
                },
            )
            .await
            .expect("seed a request log row");
        }
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;

        let server = stat(&html, "Server errors today");
        assert!(
            server.starts_with(r#"<div class="stat stat--alert">"#),
            "{server}"
        );
        assert!(
            server.contains(r#"<div class="stat__value">1</div>"#),
            "{server}"
        );
        assert!(
            html.contains("--chart-color: var(--accent-danger)"),
            "a window with a server error draws its chart in red: {html}"
        );
        assert!(
            html.contains("--chart-color: var(--text-primary)")
                && !html.contains("--chart-color: var(--accent-warning)"),
            "traffic is drawn in neutral ink, not the warning amber: {html}"
        );

        let requests = stat(&html, "Requests today");
        assert!(!requests.contains("stat--alert"), "{requests}");
        assert!(
            requests.contains(r#"<div class="stat__value">4</div>"#),
            "{requests}"
        );
        assert!(
            requests
                .contains(r#"<div class="stat__detail">2 client errors · 1 rate-limited</div>"#),
            "the client errors break the request figure down, the 429 among them: {requests}"
        );

        let card_at = html
            .find("Recent server errors")
            .expect("the server-error card");
        let card = &html[card_at..];
        assert!(card.contains("/failed"), "{card}");
        assert!(
            !card.contains("/refused-missing") && !card.contains("/refused-throttled"),
            "a client error is not a server error: {card}"
        );
        assert!(
            html.contains(r#"href="/b/admin/logs?server_errors=1""#),
            "the card links to the logs page narrowed the same way: {html}"
        );
    }

    /// No server error today: the headline reads 0 and is not red.
    #[tokio::test]
    async fn a_day_without_server_errors_is_not_an_alert() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        let server = stat(&html, "Server errors today");
        assert!(server.starts_with(r#"<div class="stat">"#), "{server}");
        assert!(
            !html.contains("--chart-color: var(--accent-danger)"),
            "no server error in the window: no red chart is drawn: {html}"
        );
        assert!(
            server.contains(r#"<div class="stat__value">0</div>"#),
            "{server}"
        );
        assert!(
            stat(&html, "Requests today").contains("0 client errors"),
            "{html}"
        );
    }

    /// An all-zero chart's sentence follows how far the records go back. A
    /// request log that began 40 days ago with nothing in the window since
    /// says "No requests in the last 30 days" — zero is data — and never
    /// "recorded yet".
    #[tokio::test]
    async fn chart_sentences_follow_the_oldest_record_not_the_first_non_zero_day() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let fresh = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        for counts in ["new users", "requests", "server errors", "client errors"] {
            assert!(
                fresh.contains(&format!(
                    r#"<p class="empty-note">No {counts} recorded yet</p>"#
                )),
                "no accounts and no request log yet: the {counts} chart says so: {fresh}"
            );
        }

        let at = (chrono::Utc::now() - chrono::Duration::days(40)).to_rfc3339();
        request_logs::insert_at(
            &ctx,
            "old",
            &request_logs::NewRequestLog {
                method: "GET",
                path: "/",
                status_code: 200,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
            &at,
        )
        .await
        .expect("seed an old request log row");

        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        for counts in ["requests", "server errors", "client errors"] {
            assert!(
                html.contains(&format!(
                    r#"<p class="empty-note">No {counts} in the last 30 days</p>"#
                )),
                "the {counts} window is covered and all zero: {html}"
            );
        }
        assert!(
            html.contains(r#"<p class="empty-note">No new users recorded yet</p>"#),
            "only the signup chart still has no records: {html}"
        );
        assert_eq!(html.matches("recorded yet").count(), 1, "{html}");
    }

    /// A recent user's creation date is a per-run value in the visual-baseline
    /// capture of this page, which masks dates by the `<time>` element alone.
    #[tokio::test]
    async fn the_recent_users_card_renders_the_created_date_as_a_time_element() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;

        assert!(
            html.contains(r#"datetime="2026-01-01T00:00:00.000Z" title="2026-01-01T00:00:00.000Z">2026-01-01 00:00 UTC</time>"#),
            "the Recent users date must be a <time>: {html}"
        );
    }

    /// A recent user is the avatar beside the email; the avatar is
    /// decorative, so the row reads as the email alone.
    #[tokio::test]
    async fn a_recent_user_is_an_avatar_beside_the_email() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        let doc = scraper::Html::parse_document(&html);
        let cell = doc
            .select(&scraper::Selector::parse(r#"td[data-label="Email"] .recent-user"#).unwrap())
            .next()
            .unwrap_or_else(|| panic!("no recent user: {html}"));
        let markup = cell.inner_html();
        // scraper writes attributes in name order.
        assert!(
            markup.starts_with(r#"<span aria-hidden="true" class="avatar avatar--md">U</span>"#),
            "{markup}"
        );
        let cells = doc
            .select(&scraper::Selector::parse("tr").unwrap())
            .find(|row| row.inner_html().contains("recent-user"))
            .unwrap()
            .select(&scraper::Selector::parse("td").unwrap())
            .count();
        assert_eq!(
            cells, 2,
            "the email and the date; every recent user is active, so no status (rule 11): {html}"
        );
    }

    /// The recent-rows and chart panels are `Card`s, each an h2 under the
    /// page's h1. Both recent-rows cards are flush whether or not they have
    /// rows: an empty one holds the `DataTable`'s note, which takes the
    /// card's inset back, never a framed empty state.
    #[tokio::test]
    async fn the_panels_are_cards_and_the_recent_cards_are_always_flush() {
        use crate::test_support::dom;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let fresh = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        assert_eq!(
            dom::heading_outline(&fresh),
            [
                (1, "Dashboard"),
                (2, "New users"),
                (2, "Requests"),
                (2, "Server errors"),
                (2, "Client errors"),
                (2, "Recent users"),
                (2, "Recent server errors"),
            ]
            .map(|(level, text)| (level, text.to_string()))
            .to_vec(),
            "{fresh}"
        );
        let flush_head = |text: &str| {
            format!(
                r#"<section class="card card--flush"><header class="card__head"><div><h2 class="card__title">{text}"#
            )
        };
        for title in ["Recent users", "Recent server errors"] {
            assert!(fresh.contains(&flush_head(title)), "{title}: {fresh}");
        }
        assert!(
            fresh.contains(&format!(
                r#"<div class="card__body"><p class="empty-note">{NO_USERS_YET}</p></div>"#
            )) && fresh.contains(&format!(
                r#"<div class="card__body"><p class="empty-note">{NO_RECENT_SERVER_ERRORS}</p></div>"#
            )),
            "{fresh}"
        );
        assert!(dom::card_level_inside_card(&fresh).is_empty(), "{fresh}");
        // Two rows of charts, which stay 2-up down to a 45rem row, then the
        // recent pair, whose errors table needs the wider minimum.
        let rows: Vec<String> = scraper::Html::parse_document(&fresh)
            .select(&scraper::Selector::parse(".dashboard-row").unwrap())
            .map(|row| row.value().attr("class").unwrap().to_string())
            .collect();
        assert_eq!(
            rows,
            [
                "dashboard-row",
                "dashboard-row",
                "dashboard-row dashboard-row--tables"
            ],
            "{fresh}"
        );

        ctx.seed_auth_user("u-1").await;
        request_logs::insert(
            &ctx,
            &request_logs::NewRequestLog {
                method: "GET",
                path: "/failed",
                status_code: 500,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
        )
        .await
        .expect("seed a request log row");
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        assert_eq!(
            html.matches(r#"<section class="card card--flush">"#)
                .count(),
            2,
            "both recent-rows cards, now holding a table: {html}"
        );
        for title in ["Recent users", "Recent server errors"] {
            let card = &html[html.find(&flush_head(title)).unwrap()..];
            let card = &card[..card.find("</section>").unwrap()];
            assert!(
                card.contains(r#"<div class="data-table">"#) && !card.contains("empty-note"),
                "{title}: {card}"
            );
        }
        assert!(dom::card_level_inside_card(&html).is_empty(), "{html}");
    }

    /// Every card-head link is named for where it goes. A screen reader's
    /// links list shows the names without their cards, where four "View"
    /// and two "View all" links went to five different pages: each name now
    /// goes to one destination, and each destination has one name.
    #[tokio::test]
    async fn every_head_link_names_its_one_destination() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;
        let doc = scraper::Html::parse_document(&html);
        let links: Vec<(String, String)> = doc
            .select(&scraper::Selector::parse("a.card__link").unwrap())
            .map(|a| {
                (
                    a.text().collect::<String>(),
                    a.value().attr("href").unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            links,
            [
                ("View users", "/b/admin/users"),
                ("View logs", "/b/admin/logs"),
                ("View server errors", "/b/admin/logs?server_errors=1"),
                ("View client errors", "/b/admin/logs?client_errors=1"),
                ("View users", "/b/admin/users"),
                ("View server errors", "/b/admin/logs?server_errors=1"),
            ]
            .map(|(label, href)| (label.to_string(), href.to_string()))
            .to_vec(),
            "in card order: the four charts, then Recent users and Recent server errors"
        );
        let mut names: HashMap<&str, &str> = HashMap::new();
        let mut hrefs: HashMap<&str, &str> = HashMap::new();
        for (label, href) in &links {
            assert_eq!(
                *names.entry(label).or_insert(href),
                href,
                "{label} names two pages"
            );
            assert_eq!(
                *hrefs.entry(href).or_insert(label),
                label,
                "{href} has two names"
            );
        }
    }

    /// Three panels carry figures the visual-baseline suite masks: the "Avg
    /// response" stat, a latency measured during the run that screenshots
    /// this page, and the "Requests today" stat and the "Requests" chart,
    /// which count every request the suite made before the capture. Each is
    /// masked by its label text (`.stat:has-text("…") .stat__value`,
    /// `section.card:has(h2.card__title:text-is("Requests")) .chart`) rather
    /// than by an attribute, because `components::Stat` takes its value as a
    /// plain `&str` and the chart card offers no slot for one either. That
    /// makes each label part of the mask's contract: rename it and the mask
    /// silently stops matching, and the panel is compared pixel by pixel
    /// again with nothing announcing the change.
    ///
    /// The capture follows the suite's own traffic, so the Requests chart is
    /// drawn there; the test seeds a request to draw it too.
    #[tokio::test]
    async fn the_masked_panels_keep_the_labels_the_visual_masks_key_on() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        request_logs::insert(
            &ctx,
            &request_logs::NewRequestLog {
                method: "GET",
                path: "/served",
                status_code: 200,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.7",
                user_id: "",
            },
        )
        .await
        .expect("seed a request log row");
        let html = output_html(dashboard(&ctx, &admin_msg("retrieve", "/b/admin/")).await).await;

        for stat in ["Avg response", "Requests today"] {
            let label = format!(r#"<div class="stat__label">{stat}</div>"#);
            let at = html.find(&label).unwrap_or_else(|| {
                panic!("visual-baseline.spec.ts masks this stat by this exact label, which is gone: {label} in {html}")
            });
            let rest = &html[at + label.len()..];
            assert!(
                rest.starts_with(r#"<div class="stat__value">"#),
                "the masked element is the `.stat__value` that follows the label: {rest:.120}"
            );
        }

        let doc = scraper::Html::parse_document(&html);
        let selector = |s: &str| scraper::Selector::parse(s).unwrap();
        let requests_charts = doc
            .select(&selector("section.card"))
            .filter(|card| {
                card.select(&selector("h2.card__title"))
                    .any(|title| title.text().collect::<String>() == "Requests")
            })
            .filter(|card| card.select(&selector(".chart")).next().is_some())
            .count();
        assert_eq!(
            requests_charts, 1,
            "visual-baseline.spec.ts masks the `.chart` of the one card titled exactly \"Requests\": {html}"
        );
    }
}

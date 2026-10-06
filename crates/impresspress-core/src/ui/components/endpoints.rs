//! The endpoint reference: a block's declared HTTP surface as a table.

use maud::{html, Markup};
use wafer_run::BlockEndpoint;

use super::{badge, breakable_id, BadgeVariant, DataTable, TableCol, TableRow};

const COLUMNS: [TableCol<'static>; 4] = [
    TableCol::new("Path"),
    TableCol::new("Method"),
    TableCol::new("Description"),
    TableCol::new("Access"),
];

/// `endpoints` as a grid named `label`: the path, the method and the access
/// level as badges ([`BadgeVariant::for_method`], [`BadgeVariant::for_auth`]),
/// and the endpoint's summary.
///
/// One rendering for every place the admin lists endpoints — a block's own
/// Endpoints page and the block detail modal — so a method or an access level
/// reads in the same colour wherever it appears. Callers pass the
/// [`BlockEndpoint`]s their route table declares (`endpoint_match::declare`),
/// never a hand-written list that can drift from what the block serves.
///
/// A reference grid rather than a list of records, so it stays a grid at
/// every width inside a labelled, focusable scroller ([`DataTable::scroll`]):
/// a keyboard user can scroll it, and `label` tells two tables on one page
/// apart.
pub fn endpoint_table(label: &str, endpoints: &[BlockEndpoint]) -> Markup {
    let rows = endpoints
        .iter()
        .map(|ep| {
            TableRow::new(vec![
                html! { code { (breakable_id(&ep.path)) } },
                badge(BadgeVariant::for_method(ep.method), &ep.method.to_string()),
                html! { (ep.summary) },
                badge(BadgeVariant::for_auth(ep.auth), &ep.auth.to_string()),
            ])
        })
        .collect();
    DataTable::new(&COLUMNS).rows(rows).scroll(label).render()
}

#[cfg(test)]
mod tests {
    use wafer_run::{AuthLevel, HttpMethod};

    use super::*;

    #[test]
    fn methods_and_access_levels_render_their_tone_classes() {
        for (method, class) in [
            (HttpMethod::Get, "badge--tone-brand"),
            (HttpMethod::Post, "badge--tone-green"),
            (HttpMethod::Patch, "badge--tone-amber"),
            (HttpMethod::Delete, "badge--tone-red"),
        ] {
            assert_eq!(
                badge(BadgeVariant::for_method(method), "x").into_string(),
                format!(r#"<span class="badge {class}">x</span>"#),
                "{method:?}"
            );
        }
        for (auth, class) in [
            (AuthLevel::Public, "badge--tone-green"),
            (AuthLevel::Admin, "badge--tone-red"),
            (AuthLevel::Authenticated, "badge--tone-amber"),
        ] {
            assert_eq!(
                badge(BadgeVariant::for_auth(auth), "x").into_string(),
                format!(r#"<span class="badge {class}">x</span>"#),
                "{auth:?}"
            );
        }
    }

    #[test]
    fn each_endpoint_is_a_row_in_a_labelled_focusable_scroller() {
        let html = endpoint_table(
            "Thing endpoints",
            &[
                BlockEndpoint::get("/b/thing/items").summary("List items"),
                BlockEndpoint::post("/b/thing/items")
                    .summary("Create an item")
                    .auth(AuthLevel::Admin),
            ],
        )
        .into_string();
        assert_eq!(html.matches("data-table__row").count(), 2, "{html}");
        assert!(
            html.contains(r#"role="region" tabindex="0" aria-label="Thing endpoints""#),
            "{html}"
        );
        assert!(html.contains(">List items<"), "{html}");
        assert!(
            html.contains(r#"<span class="badge badge--tone-green">POST</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge badge--tone-red">admin</span>"#),
            "{html}"
        );
    }
}

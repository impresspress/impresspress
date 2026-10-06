//! The endpoint reference: a block's declared HTTP surface as a table.

use maud::{html, Markup};
use wafer_run::{AuthLevel, BlockEndpoint, HttpMethod};

use super::{badge, breakable_id, DataTable, TableCol, TableRow};
use crate::ui::components::BadgeVariant;

const COLUMNS: [TableCol<'static>; 4] = [
    TableCol::new("Path").primary(),
    TableCol::new("Method"),
    TableCol::new("Description"),
    TableCol::new("Access"),
];

/// `endpoints` as a [`DataTable`]: the path (the card title on a phone), the
/// method and the access level as badges, and the endpoint's summary.
///
/// One rendering for every place the admin lists endpoints — a block's own
/// Endpoints page and the block detail modal — so a method or an access level
/// reads in the same colour wherever it appears. Callers pass the
/// [`BlockEndpoint`]s their route table declares (`endpoint_match::declare`),
/// never a hand-written list that can drift from what the block serves.
pub fn endpoint_table(endpoints: &[BlockEndpoint]) -> Markup {
    let rows = endpoints
        .iter()
        .map(|ep| {
            TableRow::new(vec![
                html! { code { (breakable_id(&ep.path)) } },
                badge(method_tone(ep.method), &ep.method.to_string()),
                html! { (ep.summary) },
                badge(auth_tone(ep.auth), &ep.auth.to_string()),
            ])
        })
        .collect();
    DataTable::new(&COLUMNS).rows(rows).render()
}

/// The badge colour of an HTTP method. Shares its colour set with
/// [`auth_tone`] — `Post`/`Public` and `Patch`/`Authenticated` render
/// identically, so the tones live once in `styles/components/badge.css`
/// rather than being declared per enum.
fn method_tone(method: HttpMethod) -> BadgeVariant {
    match method {
        HttpMethod::Get => BadgeVariant::ToneBrand,
        HttpMethod::Post => BadgeVariant::ToneGreen,
        HttpMethod::Patch => BadgeVariant::ToneAmber,
        HttpMethod::Delete => BadgeVariant::ToneRed,
    }
}

/// The badge colour of an access level. See [`method_tone`].
fn auth_tone(auth: AuthLevel) -> BadgeVariant {
    match auth {
        AuthLevel::Public => BadgeVariant::ToneGreen,
        AuthLevel::Admin => BadgeVariant::ToneRed,
        AuthLevel::Authenticated => BadgeVariant::ToneAmber,
    }
}

#[cfg(test)]
mod tests {
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
                badge(method_tone(method), "x").into_string(),
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
                badge(auth_tone(auth), "x").into_string(),
                format!(r#"<span class="badge {class}">x</span>"#),
                "{auth:?}"
            );
        }
    }

    #[test]
    fn each_endpoint_is_a_row_titled_by_its_path() {
        let html = endpoint_table(&[
            BlockEndpoint::get("/b/thing/items").summary("List items"),
            BlockEndpoint::post("/b/thing/items")
                .summary("Create an item")
                .auth(AuthLevel::Admin),
        ])
        .into_string();
        assert_eq!(html.matches("data-table__row").count(), 2, "{html}");
        assert!(html.contains("data-table__cell--primary"), "{html}");
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

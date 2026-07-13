//! SVG icon functions for use in maud templates.
//!
//! All icons are 18x18 by default (matching the old lucide-preact size).
//! Each returns a `maud::Markup` that renders an inline SVG.

use maud::{Markup, PreEscaped};

fn icon(svg_inner: &str) -> Markup {
    PreEscaped(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{svg_inner}</svg>"#
    ))
}

pub fn users() -> Markup {
    icon(
        r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>"#,
    )
}

pub fn shield() -> Markup {
    icon(
        r#"<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/>"#,
    )
}

pub fn key() -> Markup {
    icon(
        r#"<path d="m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4"/><path d="m21 2-9.6 9.6"/><circle cx="7.5" cy="15.5" r="5.5"/>"#,
    )
}

pub fn settings() -> Markup {
    icon(
        r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"#,
    )
}

pub fn layout_dashboard() -> Markup {
    icon(
        r#"<rect width="7" height="9" x="3" y="3" rx="1"/><rect width="7" height="5" x="14" y="3" rx="1"/><rect width="7" height="9" x="14" y="12" rx="1"/><rect width="7" height="5" x="3" y="16" rx="1"/>"#,
    )
}

pub fn file_text() -> Markup {
    icon(
        r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/>"#,
    )
}

pub fn package() -> Markup {
    icon(
        r#"<path d="m7.5 4.27 9 5.15"/><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"/><path d="m3.3 7 8.7 5 8.7-5"/><path d="M12 22V12"/>"#,
    )
}

pub fn shopping_cart() -> Markup {
    icon(
        r#"<circle cx="8" cy="21" r="1"/><circle cx="19" cy="21" r="1"/><path d="M2.05 2.05h2l2.66 12.42a2 2 0 0 0 2 1.58h9.78a2 2 0 0 0 1.95-1.57l1.65-7.43H5.12"/>"#,
    )
}

pub fn server() -> Markup {
    icon(
        r#"<rect width="20" height="8" x="2" y="2" rx="2" ry="2"/><rect width="20" height="8" x="2" y="14" rx="2" ry="2"/><line x1="6" x2="6.01" y1="6" y2="6"/><line x1="6" x2="6.01" y1="18" y2="18"/>"#,
    )
}

pub fn log_out() -> Markup {
    icon(
        r#"<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><polyline points="16 17 21 12 16 7"/><line x1="21" x2="9" y1="12" y2="12"/>"#,
    )
}

pub fn search() -> Markup {
    icon(r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#)
}

pub fn plus() -> Markup {
    icon(r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#)
}

pub fn edit() -> Markup {
    icon(
        r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/>"#,
    )
}

pub fn trash() -> Markup {
    icon(
        r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/>"#,
    )
}

pub fn x() -> Markup {
    icon(r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#)
}

pub fn chevron_left() -> Markup {
    icon(r#"<path d="m15 18-6-6 6-6"/>"#)
}

pub fn chevron_right() -> Markup {
    icon(r#"<path d="m9 18 6-6-6-6"/>"#)
}

pub fn chevron_up() -> Markup {
    icon(r#"<path d="m18 15-6-6-6 6"/>"#)
}

pub fn chevron_down() -> Markup {
    icon(r#"<path d="m6 9 6 6 6-6"/>"#)
}

pub fn refresh_cw() -> Markup {
    icon(
        r#"<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>"#,
    )
}

pub fn menu() -> Markup {
    icon(
        r#"<line x1="4" x2="20" y1="12" y2="12"/><line x1="4" x2="20" y1="6" y2="6"/><line x1="4" x2="20" y1="18" y2="18"/>"#,
    )
}

pub fn eye() -> Markup {
    icon(
        r#"<path d="M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0"/><circle cx="12" cy="12" r="3"/>"#,
    )
}

pub fn eye_off() -> Markup {
    icon(
        r#"<path d="M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49"/><path d="M14.084 14.158a3 3 0 0 1-4.242-4.242"/><path d="M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143"/><path d="m2 2 20 20"/>"#,
    )
}

pub fn copy() -> Markup {
    icon(
        r#"<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#,
    )
}

pub fn dollar_sign() -> Markup {
    icon(
        r#"<line x1="12" x2="12" y1="2" y2="22"/><path d="M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/>"#,
    )
}

pub fn bar_chart() -> Markup {
    icon(
        r#"<line x1="12" x2="12" y1="20" y2="10"/><line x1="18" x2="18" y1="20" y2="4"/><line x1="6" x2="6" y1="20" y2="16"/>"#,
    )
}

pub fn folder() -> Markup {
    icon(
        r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#,
    )
}

pub fn user() -> Markup {
    icon(r#"<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>"#)
}

pub fn globe() -> Markup {
    icon(
        r#"<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>"#,
    )
}

pub fn robot() -> Markup {
    icon(
        r#"<path d="M12 8V4H8"/><rect width="16" height="12" x="4" y="8" rx="2"/><path d="M2 14h2"/><path d="M20 14h2"/><path d="M15 13v2"/><path d="M9 13v2"/>"#,
    )
}

pub fn network() -> Markup {
    icon(
        r#"<rect x="16" y="16" width="6" height="6" rx="1"/><rect x="2" y="16" width="6" height="6" rx="1"/><rect x="9" y="2" width="6" height="6" rx="1"/><path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3"/><path d="M12 12V8"/>"#,
    )
}

pub fn arrow_down_left() -> Markup {
    icon(r#"<path d="M17 7 7 17"/><path d="M17 17H7V7"/>"#)
}

pub fn arrow_up_right() -> Markup {
    icon(r#"<path d="M7 17 17 7"/><path d="M7 7h10v10"/>"#)
}

pub fn filter() -> Markup {
    icon(r#"<polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3"/>"#)
}

pub fn hard_drive() -> Markup {
    icon(
        r#"<line x1="22" x2="2" y1="12" y2="12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/><line x1="6" x2="6.01" y1="16" y2="16"/><line x1="10" x2="10.01" y1="16" y2="16"/>"#,
    )
}

pub fn lock() -> Markup {
    icon(
        r#"<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>"#,
    )
}

pub fn info() -> Markup {
    icon(r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"#)
}

/// `?`-in-circle glyph. Used by `ui::sidebar::nav_icon` as the visibly
/// distinct "unknown icon name" marker — don't reuse it for a nav entry's
/// own icon or the drift signal disappears.
pub fn help_circle() -> Markup {
    icon(
        r#"<circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/>"#,
    )
}

pub fn database() -> Markup {
    icon(
        r#"<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5V19A9 3 0 0 0 21 19V5"/><path d="M3 12A9 3 0 0 0 21 12"/>"#,
    )
}

pub fn link() -> Markup {
    icon(
        r#"<path d="m10 13-4.35 4.35a2 2 0 0 0 0 2.83l.6.6a2 2 0 0 0 2.83 0L13 16.66m6-6-4.35-4.35a2 2 0 0 0-2.83 0l-.6.6a2 2 0 0 0 0 2.83L16.66 13"/><line x1="9" x2="15" y1="15" y2="9"/>"#,
    )
}

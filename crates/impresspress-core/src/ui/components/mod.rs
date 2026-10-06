//! Shared UI components. One file per family; the CSS for each lives at
//! `ui/styles/components/<same-name>.css`.

mod auth;
mod avatar;
mod badge;
mod button;
mod callout;
mod card;
mod chart;
mod empty;
mod endpoints;
mod form;
mod modal;
mod pagination;
mod section;
mod stat;
mod table;
mod time;

pub use auth::{alert, alert_message, auth_panel, oauth_button, AlertVariant};
pub use avatar::{avatar, CtrlSize};
pub use badge::{badge, status_badge, Badge, BadgeVariant};
pub use button::{button, filter_toggle, subnav, tab_navigation, BtnVariant, Tab};
pub use callout::{callout, CalloutTone};
pub use card::page_header;
pub use chart::{bar_chart_card, line_chart_card, sparkline, ChartHistory};
pub use empty::empty_state;
pub use endpoints::endpoint_table;
pub use form::{
    password_field, reveal_toggle, search_input, search_input_with_value, PasswordPurpose,
};
pub use modal::{modal, modal_cancel, modal_footer, Modal, ModalSize};
pub use pagination::pagination;
pub use section::section_header;
pub use stat::{stat_card, StatCard};
pub(crate) use table::fnv1a;
pub use table::{breakable_id, data_table, DataTable, TableCol, TableRow, NO_VALUE};
pub use time::timestamp;

//! Shared UI components. One file per family; the CSS for each lives at
//! `ui/styles/components/<same-name>.css`.

mod auth;
mod avatar;
mod badge;
mod button;
mod card;
mod chart;
mod empty;
mod form;
mod modal;
mod pagination;
mod stat;
mod table;
mod time;

pub use auth::{alert, auth_panel, oauth_button, AlertVariant};
pub use avatar::{avatar, CtrlSize};
pub use badge::{badge, status_badge, Badge, BadgeVariant};
pub use button::{button, tab_navigation, BtnVariant, Tab};
pub use card::page_header;
pub use chart::{bar_chart_card, line_chart_card, sparkline};
pub use empty::empty_state;
pub use form::{search_input, search_input_with_value};
pub use modal::modal;
pub use pagination::pagination;
pub use stat::stat_card;
pub use table::{breakable_id, data_table, DataTable, TableCol, TableRow};
pub use time::timestamp;

//! Shared UI components. One file per family; the CSS for each lives at
//! `ui/styles/components/<same-name>.css`.

mod action;
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

pub use action::{Action, ActionTarget, HeaderAction};
pub use auth::{alert, alert_message, auth_panel, oauth_button, AlertVariant};
pub use avatar::{avatar, user_identity, AvatarSize};
pub use badge::{badge, humanize, status_badge, Badge, BadgeVariant, HeaderStatus};
pub use button::{filter_links, filter_toggle, subnav, tab_navigation, FilterLinks, Tab};
pub use callout::{callout, CalloutTone};
pub use card::Card;
pub use chart::{ChartCard, ChartHistory};
pub use empty::{empty_note, empty_state};
pub use endpoints::endpoint_table;
pub use form::{password_field, reveal_toggle, FilterInput, PasswordPurpose, SearchInput};
pub use modal::{modal, modal_cancel, modal_footer, Modal, ModalSize};
pub use pagination::pagination;
pub use section::section_header;
pub use stat::{stat_row, Stat, MAX_STATS};
pub(crate) use table::fnv1a;
pub use table::{breakable_id, data_table, DataTable, TableCol, TableRow, NO_VALUE};
pub use time::timestamp;

//! `impresspress/email`: a service block with no HTTP pages.

use super::Entry;

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/email",
        fixture: None,
        exempt: &[],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

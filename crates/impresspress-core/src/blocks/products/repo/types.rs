//! Table declaration for the product-type taxonomy.
//!
//! Row-level access is the block's generic CRUD (`blocks::crud`), which takes
//! the table from its caller: `handlers/types.rs` is the only caller and it
//! runs no query this module could own that `crud` does not already express.
//! This module exists to own the name per the `repo`-module-owns-its-`TABLE`
//! convention, the same way `repo/stripe_events.rs` does.
pub(crate) const TABLE: &str = "impresspress__products__types";

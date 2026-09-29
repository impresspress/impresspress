//! Data-access layer for the LLM orchestrator block.
//!
//! Each submodule owns its table name, the column names and the row shape
//! (the canonical `repo`-module-owns-its-`TABLE` convention), and is the sole
//! place that issues `db::*` against that table. Handlers keep HTTP,
//! authorization and provider-routing policy at the call site.
//!
//! The block's other table, `impresspress__llm__providers`, is encoded and
//! decoded by `schema.rs` and read by `providers/`; it joins this module when
//! that pair is untangled.

pub(crate) mod settings;

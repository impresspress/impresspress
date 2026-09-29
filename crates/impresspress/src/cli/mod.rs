//! Unified CLI surface.
//!
//! `cli_args` + `mode` define the parser and mode/target detection.
//! `flows` contains the four (mode × target) handlers. `helpers` holds
//! the cross-flow utilities (block discovery, frontend copy, overlay
//! application, wasm resolution, static-file HTTP server). `config` is
//! the `impresspress.toml` schema + walk-up loader. The in-process native
//! server boot the sealed × native flow runs is `impresspress_server::run`.
//! `cmd` is the child-process runner used by the
//! flows that shell out (cargo, wasm-pack, wafer).
pub mod cli_args;
pub mod cmd;
pub mod config;
pub mod flows;
pub mod helpers;
pub mod mode;

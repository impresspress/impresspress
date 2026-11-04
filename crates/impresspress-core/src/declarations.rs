//! Reading block declarations however they are held.
//!
//! A runtime holds each block's `BlockInfo` once, as the `Arc` its
//! registration read (wafer-run#478), and lends those `Arc`s —
//! `Wafer::block_infos()`, `Context::registered_blocks()`. A test, or a pass
//! over declarations no runtime holds yet, has owned `BlockInfo`s. The
//! functions that read declarations (routing, enablement, discovery, request
//! logging) are generic over `B: Borrow<BlockInfo>` so both pass as they are,
//! and none copies a declaration to read it.

use std::borrow::Borrow;

use wafer_run::BlockInfo;

/// The declarations in `blocks`, borrowed.
pub fn declared<B: Borrow<BlockInfo>>(
    blocks: &[B],
) -> impl Iterator<Item = &BlockInfo> + Clone + '_ {
    blocks.iter().map(|block| block.borrow())
}

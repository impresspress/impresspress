//! `impresspress_feature_block!` — declarative scaffolding for impresspress feature
//! blocks.
//!
//! Every `impresspress/*` feature block repeated the same boilerplate around its
//! genuinely-custom `info()` / `handle()` bodies:
//!
//! - a unit (or single-field) struct + `new()` + `Default` delegating to `new()`;
//! - the two-line `#[cfg_attr(..., async_trait::async_trait)]` /
//!   `#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]`
//!   pair on the `impl Block` (now collapsed to wafer-block-macro's
//!   [`wafer_async_trait`](wafer_block::wafer_async_trait));
//! - a `lifecycle(Init)` body that runs the block's migrations (now folded into
//!   [`crate::migration_helper::lifecycle_init`]).
//!
//! This macro is the impresspress analogue of wafer-core's `service_block!`
//! (W3-O): it owns the scaffolding so each block file carries only its `info`,
//! `handle`, and (optional) `lifecycle` expressions. Registration is **not**
//! generated here — it comes from the single `(feature, name, constructor)`
//! manifest in [`crate::blocks`] (see `register_feature_blocks` /
//! `all_block_infos`), so there is exactly one place enumerating the block set.
//!
//! # Forms
//!
//! ```ignore
//! // Unit-struct block. `info`/`handle` are expression bindings inlined into
//! // the generated method bodies, so `.await` / `?` work directly.
//! impresspress_feature_block! {
//!     /// Doc comment for the struct.
//!     pub struct VectorBlock;
//!     name: "impresspress/vector",
//!     info: |this| { /* -> BlockInfo */ },
//!     handle: |this, ctx, msg, input| { /* -> OutputStream */ },
//!     lifecycle: |this, ctx, event| { /* -> Result<(), WaferError> */ },
//! }
//!
//! // Field-bearing block. `fields` become private struct fields, initialized
//! // from `Default::default()` in `new()`.
//! impresspress_feature_block! {
//!     pub struct FilesBlock;
//!     fields: { limiter: UserRateLimiter },
//!     name: "impresspress/files",
//!     info: |this| { /* ... */ },
//!     handle: |this, ctx, msg, input| { /* ... */ },
//!     lifecycle: |this, ctx, event| { /* ... */ },
//! }
//! ```
//!
//! The `info`/`handle`/`lifecycle` bindings look like closures but are not:
//! each is inlined into the generated method body, so the block module's own
//! `use` imports are in scope and `?`/`.await` resolve against the method's
//! signature. Bindings the body doesn't use must be spelled with a leading
//! underscore (`_this`, `_ctx`), exactly like unused function parameters.
//!
//! `lifecycle` is optional; when omitted the `Block` trait's no-op default is
//! used (the embedding wrappers, which have no migrations, rely on this).

/// Support module re-exporting every item the [`impresspress_feature_block!`]
/// expansion references through `$crate::…` paths. Hidden: it is an expansion
/// detail, not public API.
#[doc(hidden)]
pub mod __private {
    pub use wafer_block::wafer_async_trait;
    pub use wafer_run::{
        context::Context, Block, BlockInfo, InputStream, LifecycleEvent, Message, OutputStream,
        WaferError,
    };
}

/// Generate a impresspress feature block's struct, constructor, `Default`, and
/// `impl Block` scaffolding. See the [module docs](self) for the forms and the
/// binding semantics.
#[macro_export]
macro_rules! impresspress_feature_block {
    (
        $(#[$attr:meta])*
        $vis:vis struct $block:ident;
        $(fields: { $($field:ident : $fty:ty),+ $(,)? },)?
        name: $name:literal,
        info: |$ithis:ident| $iexpr:expr,
        handle: |$hthis:ident, $hctx:ident, $hmsg:ident, $hinput:ident| $hexpr:expr,
        $(lifecycle: |$lthis:ident, $lctx:ident, $levent:ident| $lexpr:expr,)?
    ) => {
        $(#[$attr])*
        $vis struct $block {
            $($($field: $fty,)+)?
        }

        impl $block {
            #[doc = concat!("Construct a [`", stringify!($block), "`].")]
            pub fn new() -> Self {
                Self {
                    $($($field: ::core::default::Default::default(),)+)?
                }
            }
        }

        impl ::core::default::Default for $block {
            fn default() -> Self {
                Self::new()
            }
        }

        #[$crate::blocks::feature_block::__private::wafer_async_trait]
        impl $crate::blocks::feature_block::__private::Block for $block {
            fn info(&self) -> $crate::blocks::feature_block::__private::BlockInfo {
                let $ithis = self;
                $iexpr
            }

            async fn handle(
                &self,
                ctx: &dyn $crate::blocks::feature_block::__private::Context,
                msg: $crate::blocks::feature_block::__private::Message,
                input: $crate::blocks::feature_block::__private::InputStream,
            ) -> $crate::blocks::feature_block::__private::OutputStream {
                let $hthis = self;
                let $hctx = ctx;
                // Some blocks mutate `msg` in their dispatch (e.g. messages'
                // `endpoint_match::dispatch(&mut msg, …)`); others don't. Bind
                // `mut` unconditionally and silence the lint for the
                // non-mutating blocks.
                #[allow(unused_mut)]
                let mut $hmsg = msg;
                let $hinput = input;
                $hexpr
            }

            $(
                async fn lifecycle(
                    &self,
                    ctx: &dyn $crate::blocks::feature_block::__private::Context,
                    event: $crate::blocks::feature_block::__private::LifecycleEvent,
                ) -> ::std::result::Result<
                    (),
                    $crate::blocks::feature_block::__private::WaferError,
                > {
                    let $lthis = self;
                    let $lctx = ctx;
                    let $levent = event;
                    $lexpr
                }
            )?
        }

        impl $block {
            /// The block's registered name (`{org}/{block}`). The single
            /// `(feature, name, constructor)` manifest in [`crate::blocks`]
            /// references this so a block's declared `info().name` and its
            /// registration key can't drift.
            pub const BLOCK_NAME: &'static str = $name;
        }
    };
}

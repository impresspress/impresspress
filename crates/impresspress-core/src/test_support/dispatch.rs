//! What [`super::TestContext`] needs to dispatch a `call_block` the way
//! `wafer_run::context::RuntimeContext` does: who a frame's caller is, and
//! the WRAP grant set a deployment runs with. The admission gates themselves
//! are not here: `TestContext::call_block` runs the runtime's own
//! `wafer_run::runtime::call_gates::admit_call`.
//!
//! The grant set is not assembled here either. A real [`wafer_run::Wafer`]
//! collects it — the registration functions `ImpresspressBuilder::build`
//! calls, then `Wafer::register_block` for each block the fixture registered
//! and `Wafer::add_wrap_grants` for the deployment's own. A grant the runtime
//! rejects (a typed Network grant from a non-admin block, a namespace grant
//! on a resource the block does not own, a malformed resource) is refused
//! here as boot refuses it: [`refuse_rejected_grants`] panics with the
//! runtime's own `GrantsRejected` text.

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

use wafer_run::{Block, ResourceGrant};

/// Whose code runs on a [`super::TestContext`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Frame {
    /// What every constructor hands back: nobody's code. A service it calls
    /// sees no caller, and the runtime refuses an unattributed caller every
    /// table, key and object a WRAP check guards — so code under test run
    /// here is refused until the test says whose code it is
    /// ([`super::TestContext::running_as`]), and setup until the test says
    /// it is setup ([`super::TestContext::fixture`]).
    Unframed,
    /// Test setup: the migrations, seeds and read-backs a test stages and
    /// asserts through. See [`Caller::Fixture`].
    Fixture,
    /// A block's code.
    Block(String),
}

impl Frame {
    /// Who a block this frame calls sees as its caller.
    pub(super) fn as_caller(&self) -> Caller {
        match self {
            Frame::Unframed => Caller::Nobody,
            Frame::Fixture => Caller::Fixture,
            Frame::Block(name) => Caller::Block(name.clone()),
        }
    }
}

/// Who called into a [`super::TestContext`] frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Caller {
    /// Nobody: a top-level frame — the context `Wafer::run_block` builds for
    /// the block a listener or flow step dispatches to, which has no caller.
    /// A block entered through [`super::TestContext::running_as`] runs here.
    /// A block the router reaches is not top-level: `impresspress/router`
    /// calls it, and [`super::TestContext::dispatch_resolved`] routes the same
    /// way.
    /// Also the caller of anything an unframed context ([`Frame::Unframed`])
    /// calls.
    Nobody,
    /// The test itself, calling from [`Frame::Fixture`]: the migrations,
    /// seeds and read-backs a test sets up and asserts with.
    ///
    /// No production frame corresponds to this. It is authorized as the
    /// admin block, the one identity WRAP exempts, so fixture setup can
    /// write any table; code whose permissions are under test must run in a
    /// block's frame instead — [`super::TestContext::running_as`], a routed
    /// [`super::TestContext::dispatch_resolved`], or a block reached through
    /// `call_block`.
    Fixture,
    /// The block whose frame made the call.
    Block(String),
}

impl Caller {
    /// [`wafer_run::context::Context::caller_id`]: the calling block, and no
    /// block for the other two.
    pub(super) fn block(&self) -> Option<&str> {
        match self {
            Caller::Block(name) => Some(name),
            Caller::Nobody | Caller::Fixture => None,
        }
    }

    /// The identity a WRAP check authorizes: the calling block, the admin
    /// block for the fixture itself, and nobody for a top-level frame —
    /// which `wafer_block::wrap::check_access` refuses, as the runtime does.
    pub(super) fn wrap_identity(&self) -> Option<&str> {
        match self {
            Caller::Block(name) => Some(name),
            Caller::Fixture => Some(crate::blocks::admin::ADMIN_BLOCK_ID),
            Caller::Nobody => None,
        }
    }
}

/// An empty runtime: no linked-in blocks, no lockfile, the admin block set
/// as `ImpresspressBuilder::build` sets it.
pub(super) fn empty_wafer() -> wafer_run::Wafer {
    let mut wafer = wafer_run::Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("an empty runtime builds");
    wafer.set_admin_block(crate::blocks::admin::ADMIN_BLOCK_ID);
    wafer
}

/// Register `block` under `name` the way the runtime does and hand back the
/// `BlockInfo` it admitted, or panic with the runtime's refusal.
///
/// `Wafer::register_block` is the admission every registration path goes
/// through: the name must be a legal block name, the block must report the
/// name it is registered under, and its declaration must pass
/// `BlockInfo::validate` — reserved or foreign config keys, illegal agent
/// tool names. Each of those is fatal at boot, so a fixture that admitted
/// them would certify a deployment that never starts.
///
/// The same goes for its grant declarations: a grant `Wafer::seal` would
/// refuse boot over is refused here ([`refuse_rejected_grants`]).
pub(super) fn admit(name: &str, block: Arc<dyn Block>) -> wafer_run::BlockInfo {
    let mut wafer = empty_wafer();
    let info = block.info();
    wafer
        .register_block(name, block)
        .unwrap_or_else(|e| panic!("the runtime refuses to register {name}: {e}"));
    refuse_rejected_grants(&wafer);
    info
}

/// Panic with the runtime's own refusal if `wafer` holds a grant declaration
/// `Wafer::seal` would refuse boot over (`RuntimeError::GrantsRejected`):
/// the runtime leaves such a grant out of `Wafer::wrap_grants` and refuses
/// to start, so a fixture that only collected the accepted grants would run
/// a deployment production never boots.
fn refuse_rejected_grants(wafer: &wafer_run::Wafer) {
    let rejected = wafer.rejected_grants();
    if !rejected.is_empty() {
        panic!(
            "the runtime refuses to boot: {}",
            wafer_run::RuntimeError::GrantsRejected(rejected.to_vec())
        );
    }
}

/// A block that is nothing but its declaration — what
/// [`super::TestContext::register_block_info`] admits a sandbox guest's
/// `BlockInfo` through. It is registered only in the throwaway runtime
/// [`admit`] builds, never dispatched to.
pub(super) struct Declared(pub(super) wafer_run::BlockInfo);

#[wafer_block::wafer_async_trait]
impl Block for Declared {
    fn info(&self) -> wafer_run::BlockInfo {
        self.0.clone()
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        _msg: wafer_run::Message,
        _input: wafer_run::InputStream,
    ) -> wafer_run::OutputStream {
        wafer_run::OutputStream::error(wafer_run::WaferError::new(
            wafer_run::ErrorCode::Unimplemented,
            "a declared-only block has no handler",
        ))
    }
}

/// The grants the blocks `ImpresspressBuilder::build` registers declare, as
/// the runtime collects them, and the names those blocks are registered
/// under.
///
/// Registered through the builder's own functions: the feature-block
/// manifest, admin, the framework auth block over `AuthServiceImpl`, the llm
/// feature block, and the `wafer-run/llm` router. Built once per process —
/// every one of them is a pure function of the build's features.
fn built_in() -> &'static (Vec<ResourceGrant>, HashSet<String>) {
    static BUILT_IN: OnceLock<(Vec<ResourceGrant>, HashSet<String>)> = OnceLock::new();
    BUILT_IN.get_or_init(|| {
        let mut wafer = empty_wafer();
        crate::blocks::register_feature_blocks(&mut wafer).expect("feature blocks register");
        crate::blocks::register_admin(&mut wafer, Arc::default()).expect("admin registers");
        crate::blocks::register_auth(&mut wafer).expect("auth registers");
        #[cfg(feature = "block-llm")]
        crate::blocks::register_llm(
            &mut wafer,
            Arc::new(crate::blocks::llm::provider_admin::NoopProviderAdmin),
        )
        .expect("llm registers");
        wafer_core::service_blocks::llm::register_with(
            &mut wafer,
            Arc::new(crate::builder::granted_llm_router()),
        )
        .expect("the llm router registers");
        refuse_rejected_grants(&wafer);
        let names = wafer.block_names().into_iter().collect();
        (wafer.wrap_grants().to_vec(), names)
    })
}

/// The declared allowlist production installs on `name`'s frame when the
/// fixture holds no block by that name: the built-in block's own
/// declaration, or `None` — unrestricted — for a name no built-in block
/// carries, as `Wafer::resolve_block_requires_uncached` answers for a name it
/// has no block under.
pub(super) fn built_in_call_allowlist(name: &str) -> Option<Vec<String>> {
    crate::blocks::all_block_infos()
        .into_iter()
        .find(|info| info.name == name)
        .and_then(|info| info.call_allowlist())
}

/// Every grant WRAP sees in a deployment made of the built-in blocks, the
/// fixture's `blocks` and the deployment's own `grants`.
///
/// A fixture block registered under a built-in name stands in for that block
/// and adds nothing: the built-in's declaration is the one production reads.
pub(super) fn collect_wrap_grants(
    blocks: &[(String, Arc<dyn Block>)],
    grants: &[ResourceGrant],
) -> Vec<ResourceGrant> {
    let (built_in_grants, built_in_names) = built_in();
    let mut wafer = empty_wafer();
    for (name, block) in blocks {
        if !built_in_names.contains(name) {
            wafer
                .register_block(name.as_str(), block.clone())
                .unwrap_or_else(|e| panic!("the runtime refuses to register {name}: {e}"));
        }
    }
    refuse_rejected_grants(&wafer);
    wafer
        .add_wrap_grants(grants.to_vec())
        .unwrap_or_else(|e| panic!("the runtime refuses the deployment's grants: {e}"));
    built_in_grants
        .iter()
        .cloned()
        .chain(wafer.wrap_grants().iter().cloned())
        .collect()
}

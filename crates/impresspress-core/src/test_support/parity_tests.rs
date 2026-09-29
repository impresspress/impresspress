//! [`TestContext`] answers what a sealed runtime's frame answers.
//!
//! The gates a `call_block` passes are not re-stated in the fixture: both
//! run `wafer_run::runtime::call_gates::admit_call`. What the fixture still
//! supplies itself is the frame that function reads — its depth, its alias
//! table, the caller's `requires` and capabilities — and what happens after
//! admission: the callee's frame, and the WRAP check a service makes against
//! the caller that frame names. Each case builds the same blocks twice — in
//! a real, sealed [`wafer_run::Wafer`] with the aliases and admin block
//! `ImpresspressBuilder::build` installs, and in a [`TestContext`] — enters
//! the same block in both with the same message, and asserts both answer
//! identically, so a frame answer that drifts from the runtime's fails here.

use std::sync::Arc;

use wafer_block::{codec, common::ServiceOp, wire::database::CountRequest};
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, Block, BlockCapabilities, BlockInfo,
    InputStream, Message, OutputStream, ResourceGrant,
};

use super::TestContext;

/// One call a [`Probe`] makes: `target` receives `op`. A database op counts
/// the rows of `collection`; any other op is forwarded to another probe,
/// carrying the hops left.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Hop {
    target: String,
    op: String,
    collection: Option<String>,
}

impl Hop {
    fn to(target: &str) -> Self {
        Self {
            target: target.to_string(),
            op: "probe.hop".to_string(),
            collection: None,
        }
    }

    fn count(target: &str, collection: &str) -> Self {
        Self {
            target: target.to_string(),
            op: ServiceOp::DATABASE_COUNT.to_string(),
            collection: Some(collection.to_string()),
        }
    }
}

const HOPS_META: &str = "probe.hops";

/// A block that makes the calls its message lists, one hop each, and
/// answers `ok` or the code of the first refusal with the number of hops
/// still unmade — which is what distinguishes the depth a runtime stops at.
struct Probe {
    name: &'static str,
    requires: Vec<String>,
    grants: Vec<ResourceGrant>,
    capabilities: Option<BlockCapabilities>,
}

impl Probe {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            requires: Vec::new(),
            grants: Vec::new(),
            capabilities: None,
        }
    }

    /// Unrestricted, except that it may `call_block` only `targets`.
    fn may_call_only(mut self, targets: &[&str]) -> Self {
        let mut caps = BlockCapabilities::unrestricted();
        caps.callable_blocks = wafer_block::capabilities::Allowlist::Only(
            targets.iter().map(|t| (*t).to_string()).collect(),
        );
        self.capabilities = Some(caps);
        self
    }

    fn requires(mut self, targets: &[&str]) -> Self {
        self.requires = targets.iter().map(|t| (*t).to_string()).collect();
        self
    }

    fn grants(mut self, grants: Vec<ResourceGrant>) -> Self {
        self.grants = grants;
        self
    }
}

fn message(hops: &[Hop]) -> Message {
    let mut msg = Message::new("probe.hop");
    msg.set_meta(
        HOPS_META,
        serde_json::to_string(hops).expect("hops serialize"),
    );
    msg
}

async fn outcome(out: OutputStream) -> String {
    match out.collect_buffered().await {
        Ok(buf) => String::from_utf8(buf.body).expect("utf-8 outcome"),
        Err(TerminalNotResponse::Error(e)) => format!("{:?}", e.code),
        Err(other) => panic!("a probe answered a non-response terminal: {other:?}"),
    }
}

#[wafer_block::wafer_async_trait]
impl Block for Probe {
    fn info(&self) -> BlockInfo {
        // An interface the runtime has no spec for: every probe op passes
        // the action check, which the cases that are not about it need.
        BlockInfo::new(self.name, "0.0.1", "probe@v1", "parity probe")
            .requires(self.requires.clone())
            .grants(self.grants.clone())
    }

    fn block_capabilities(&self) -> Option<BlockCapabilities> {
        self.capabilities.clone()
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let mut hops: Vec<Hop> =
            serde_json::from_str(msg.get_meta(HOPS_META)).expect("a probe message lists its hops");
        if hops.is_empty() {
            return OutputStream::respond(b"ok".to_vec());
        }
        let hop = hops.remove(0);
        let left = hops.len();
        let answer = match &hop.collection {
            Some(collection) => {
                let body = codec::encode(&CountRequest {
                    collection: collection.clone(),
                    filters: Vec::new(),
                })
                .expect("count request encodes");
                let out = ctx
                    .call_block(
                        &hop.target,
                        Message::new(hop.op.as_str()),
                        InputStream::from_bytes(body),
                    )
                    .await;
                match out.collect_buffered().await {
                    Ok(_) => "ok".to_string(),
                    Err(TerminalNotResponse::Error(e)) => format!("{:?}@{left}", e.code),
                    Err(other) => panic!("the database answered a non-response: {other:?}"),
                }
            }
            None => {
                let mut next = message(&hops);
                next.kind = hop.op.clone();
                let out = ctx
                    .call_block(&hop.target, next, InputStream::empty())
                    .await;
                let answer = outcome(out).await;
                if answer == "ok" || answer.contains('@') {
                    answer
                } else {
                    format!("{answer}@{left}")
                }
            }
        };
        OutputStream::respond(answer.into_bytes())
    }
}

/// What `entry` answers `hops` with in a sealed runtime holding `probes`,
/// and in a [`TestContext`] holding the same probes.
async fn run_both(probes: Vec<Probe>, entry: &str, hops: &[Hop]) -> (String, String) {
    let probes: Vec<Arc<Probe>> = probes.into_iter().map(Arc::new).collect();

    let mut wafer = super::dispatch::empty_wafer();
    wafer_core::service_blocks::database::register_with(
        &mut wafer,
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        ),
    )
    .expect("the database registers");
    for (alias, target) in crate::builder::SERVICE_ALIASES {
        if *target == "wafer-run/database" {
            wafer.add_alias(*alias, *target).expect("alias");
        }
    }
    for probe in &probes {
        wafer
            .register_block(probe.name, probe.clone())
            .expect("a probe registers");
    }
    wafer.seal().await.expect("the runtime seals");
    let runtime = outcome(
        wafer
            .run_block(entry, message(hops), InputStream::empty())
            .await,
    )
    .await;

    let mut ctx = TestContext::new().await;
    for probe in &probes {
        ctx.register_block(probe.name, probe.clone());
    }
    let entry_block = probes
        .iter()
        .find(|p| p.name == entry)
        .expect("the entry is a probe")
        .clone();
    let fixture = outcome(
        entry_block
            .handle(&ctx.running_as(entry), message(hops), InputStream::empty())
            .await,
    )
    .await;

    (runtime, fixture)
}

/// Each callee frame sits one level deeper than its caller, from `0` at the
/// entry, so the shared depth gate stops the same recursion at the same hop.
#[tokio::test]
async fn the_call_depth_ceiling_is_the_runtimes() {
    let hops: Vec<Hop> = (0..40).map(|_| Hop::to("test/deep")).collect();
    let (runtime, fixture) = run_both(vec![Probe::new("test/deep")], "test/deep", &hops).await;
    assert!(
        runtime.starts_with("ResourceExhausted@"),
        "the runtime stops a deep recursion: {runtime}"
    );
    assert_eq!(fixture, runtime, "the fixture stops at the same depth");
}

/// The frame carries the entry block's declared allowlist.
#[tokio::test]
async fn a_target_missing_from_requires_is_refused() {
    let (runtime, fixture) = run_both(
        vec![
            Probe::new("test/a").requires(&["test/c"]),
            Probe::new("test/b"),
            Probe::new("test/c"),
        ],
        "test/a",
        &[Hop::to("test/b")],
    )
    .await;
    assert_eq!(runtime, "PermissionDenied@0");
    assert_eq!(fixture, runtime);
}

/// The fixture's alias table ([`crate::builder::SERVICE_ALIASES`]) resolves
/// a short name to the block the builder aliases it to.
#[tokio::test]
async fn an_alias_resolves_before_the_requires_check() {
    let (runtime, fixture) = run_both(
        vec![Probe::new("test/a").requires(&["wafer-run/database"])],
        "test/a",
        &[Hop::count("db", "test__a__rows")],
    )
    .await;
    assert!(
        !runtime.starts_with("PermissionDenied") && !runtime.starts_with("Unimplemented"),
        "the runtime reaches the database through its alias: {runtime}"
    );
    assert_eq!(fixture, runtime);
}

/// The frame carries the capabilities of the block it runs as, so a call
/// its `callable_blocks` leaves out is refused — a gate the fixture had no
/// copy of before it ran the runtime's.
#[tokio::test]
async fn a_target_the_callers_capabilities_leave_out_is_refused() {
    let (runtime, fixture) = run_both(
        vec![
            Probe::new("test/a").may_call_only(&["test/c"]),
            Probe::new("test/b"),
            Probe::new("test/c"),
        ],
        "test/a",
        &[Hop::to("test/b")],
    )
    .await;
    assert_eq!(runtime, "PermissionDenied@0");
    assert_eq!(fixture, runtime);
}

/// ... and admits one it lists.
#[tokio::test]
async fn a_target_the_callers_capabilities_list_is_admitted() {
    let (runtime, fixture) = run_both(
        vec![
            Probe::new("test/a").may_call_only(&["test/c"]),
            Probe::new("test/c"),
        ],
        "test/a",
        &[Hop::to("test/c")],
    )
    .await;
    assert_eq!(runtime, "ok");
    assert_eq!(fixture, runtime);
}

/// WRAP is on in every frame: a block reading another block's table with no
/// grant is refused — however the test entered it.
#[tokio::test]
async fn an_ungranted_read_of_another_blocks_table_is_refused() {
    let (runtime, fixture) = run_both(
        vec![Probe::new("test/a"), Probe::new("test/b")],
        "test/a",
        &[Hop::count("wafer-run/database", "test__b__rows")],
    )
    .await;
    assert_eq!(runtime, "PermissionDenied@0");
    assert_eq!(fixture, runtime);
}

/// The grant the owning block declares is the one that admits the read.
#[tokio::test]
async fn the_owners_grant_admits_the_read() {
    let (runtime, fixture) = run_both(
        vec![
            Probe::new("test/a"),
            Probe::new("test/b").grants(vec![ResourceGrant::read("test/a", "test__b__rows")]),
        ],
        "test/a",
        &[Hop::count("wafer-run/database", "test__b__rows")],
    )
    .await;
    assert!(
        !runtime.starts_with("PermissionDenied"),
        "the grant admits the read: {runtime}"
    );
    assert_eq!(fixture, runtime);
}

/// A nested block's service calls are authorized as THAT block, not as the
/// block the test entered: `test/a` may read its own table, and `test/b`,
/// which `test/a` calls, may not.
#[tokio::test]
async fn a_nested_call_is_authorized_as_the_block_that_makes_it() {
    let (runtime, fixture) = run_both(
        vec![Probe::new("test/a"), Probe::new("test/b")],
        "test/a",
        &[
            Hop::to("test/b"),
            Hop::count("wafer-run/database", "test__a__rows"),
        ],
    )
    .await;
    assert_eq!(runtime, "PermissionDenied@0");
    assert_eq!(fixture, runtime);
}

/// [`TestContext::cancel`] sets the flag the shared cancellation gate reads,
/// so every later call is refused. The runtime cancels on a deadline or an
/// abort, neither of which a top-level `run_block` exposes, so this case has
/// no runtime half.
#[tokio::test]
async fn a_cancelled_fixture_refuses_further_calls() {
    let mut ctx = TestContext::new().await;
    ctx.register_block("test/a", Arc::new(Probe::new("test/a")));
    let frame = ctx.running_as("test/a");
    frame.cancel();
    assert!(frame.is_cancelled());
    let answer = outcome(
        frame
            .call_block("test/a", message(&[]), InputStream::empty())
            .await,
    )
    .await;
    assert_eq!(answer, "Cancelled");
}

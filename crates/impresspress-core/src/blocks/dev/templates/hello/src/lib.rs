//! The smallest block that serves anything: one public `GET`.
//!
//! Two functions are required of every block — `block()`, which declares what
//! the block is and what it serves, and `init()`, which runs once when the
//! block is activated. Everything else is handlers.
//!
//! The declaration is not documentation: the sandbox validates it, and the
//! capabilities it implies are the only authority the compiled block gets.
//! This one claims nothing, so it can reach nothing.

// The guest SDK. It is a crate beside the block (`../../wafer_guest`), not a
// file inside it: the compiler builds it once per session and every block
// links against that build. `export!` stamps the five entry points the host
// calls into this crate, wired to `block()` and `init()` below.
use wafer_guest::*;

wafer_guest::export!(block, init);

/// What this block is, and what it serves.
pub fn block() -> Block {
    Block::new("site/hello", "Says hello").endpoint(
        Endpoint::new(Method::Get, "/b/hello/", hello)
            .auth(Auth::Public)
            .summary("Say hello"),
    )
}

/// Runs once, when the block is activated.
///
/// This is where a block creates its tables (`db::ensure_table`) and reads
/// the config it needs. Returning `Err` fails the activation, so the block
/// never serves in a half-configured state.
pub fn init(_ctx: &Ctx) -> Result<(), String> {
    Ok(())
}

/// `GET /b/hello/`.
fn hello(_request: &Request, _ctx: &Ctx) -> Response {
    Response::text(
        200,
        "Hello from site/hello — a block compiled in your browser.",
    )
}

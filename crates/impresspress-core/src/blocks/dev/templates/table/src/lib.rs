//! A block with a table behind it: newsletter signups.
//!
//! Everything a data-backed block needs is here — a claimed collection, a
//! table created in `init`, a public write endpoint with an agent tool, and
//! two admin reads.
//!
//! # The namespace rule
//!
//! A block reaches exactly what it declares. This block is registered as
//! `site/newsletter`, so its collections are `site__newsletter__*`, its
//! storage folder would be `site/newsletter`, and its config keys would be
//! `SITE__NEWSLETTER__*`. Claiming anything outside that is refused when the
//! block is staged, with a `cap-collection` (or `cap-folder`, `cap-config`)
//! diagnostic naming the entry.
//!
//! Claiming a collection is also what turns on the `schema` capability, which
//! is what lets `db::ensure_table` create the table. Raw SQL and raw DDL are
//! never granted.

// The `cfg` gate lets the sandbox's own parity test compile this file for the
// host; a host `cargo check` fails on the unconditional `use` below. A block
// is built for wasm32-wasip1, where the module is there.
#[cfg(target_arch = "wasm32")]
mod wafer_guest;

use crate::wafer_guest::*;

/// The one table this block owns.
///
/// Named once, so the claim in `block()`, the `ensure_table` in `init()` and
/// every query below cannot drift apart — a typo in any one of them would be
/// a `PermissionDenied` at run time rather than a compile error.
const SUBSCRIBERS: &str = "site__newsletter__subscribers";

/// What this block is, what it may reach, and what it serves.
pub fn block() -> Block {
    Block::new("site/newsletter", "Newsletter signups")
        .requires(&[DATABASE])
        .collection(SUBSCRIBERS)
        .endpoint(
            Endpoint::new(Method::Post, "/b/newsletter/subscribe", subscribe)
                .auth(Auth::Public)
                .summary("Subscribe an email address")
                .input(
                    Schema::object()
                        .prop("email", Schema::string().describe("Email address"))
                        .required(&["email"]),
                )
                .output(Schema::object().prop("ok", Schema::boolean()))
                // The one endpoint an agent should be able to call. The
                // description names the side effect, because that is all the
                // agent has to decide on.
                .agent_tool(
                    "subscribe_newsletter",
                    "Subscribe an email address to the newsletter. Creates a subscriber row; \
                     duplicates are rejected.",
                ),
        )
        .endpoint(
            Endpoint::new(Method::Get, "/b/newsletter/subscribers", list_subscribers)
                .auth(Auth::Admin)
                .summary("List subscribers, newest first")
                .output(Schema::object().prop("subscribers", Schema::array(subscriber_schema()))),
        )
        .endpoint(
            Endpoint::new(
                Method::Get,
                "/b/newsletter/subscribers/{id}",
                get_subscriber,
            )
            .auth(Auth::Admin)
            .summary("Read one subscriber")
            .output(subscriber_schema()),
        )
}

/// Create the table if it is not already there.
///
/// Idempotent, and run on every activation: a block has no separate migration
/// step, so `init` is where its schema lives.
pub fn init(ctx: &Ctx) -> Result<(), String> {
    db::ensure_table(
        ctx,
        TableDef::new(SUBSCRIBERS)
            .column(Column::text("id").primary_key())
            .column(Column::text("email").not_null().unique())
            .column(Column::datetime("created_at").default_now()),
    )
    .map_err(|error| error.to_string())
}

/// The shape both read endpoints return.
fn subscriber_schema() -> Schema {
    Schema::object()
        .prop("id", Schema::string())
        .prop("email", Schema::string())
        .prop("created_at", Schema::string())
}

/// `POST /b/newsletter/subscribe` — public.
fn subscribe(request: &Request, ctx: &Ctx) -> Response {
    let body = match request.json() {
        Ok(body) => body,
        Err(detail) => return refuse(400, &format!("the body is not JSON: {detail}")),
    };
    let Some(email) = body.get("email").and_then(json::Json::as_str) else {
        return refuse(400, "`email` is required");
    };
    let email = email.trim();
    if !email.contains('@') || email.len() > 320 {
        return refuse(400, "`email` is not an email address");
    }

    // The column's UNIQUE constraint is the duplicate check: an address that
    // is already stored is refused by the insert itself, and the host names
    // that refusal `AlreadyExists`. Looking first instead would leave a gap
    // two simultaneous signups for one address can both pass.
    let row = json::Json::obj()
        .set("id", json::Json::str(&subscriber_id(email)))
        .set("email", json::Json::str(email));
    match db::create(ctx, SUBSCRIBERS, row) {
        Ok(_) => Response::json(200, &json::Json::obj().set("ok", json::Json::Bool(true))),
        Err(error) if error.code == "AlreadyExists" => {
            refuse(409, "that address is already subscribed")
        }
        Err(error) => unavailable(error),
    }
}

/// `GET /b/newsletter/subscribers` — admin.
fn list_subscribers(_request: &Request, ctx: &Ctx) -> Response {
    let options = ListOptions::new().sort("created_at", true).limit(200);
    let records = match db::list(ctx, SUBSCRIBERS, options) {
        Ok(records) => records,
        Err(error) => return unavailable(error),
    };
    let subscribers: Vec<json::Json> = records.iter().map(subscriber_view).collect();
    Response::json(
        200,
        &json::Json::obj().set("subscribers", json::Json::Arr(subscribers)),
    )
}

/// `GET /b/newsletter/subscribers/{id}` — admin.
fn get_subscriber(request: &Request, ctx: &Ctx) -> Response {
    let Some(id) = request.param("id") else {
        return refuse(400, "the path carries no subscriber id");
    };
    match db::get(ctx, SUBSCRIBERS, id) {
        Ok(record) => Response::json(200, &subscriber_view(&record)),
        Err(error) if error.code == "NotFound" => refuse(404, "no such subscriber"),
        Err(error) => unavailable(error),
    }
}

/// Project one stored record into the shape the read endpoints publish.
///
/// A record is `{"id": …, "data": {…}}` — the id is the primary key and
/// `data` holds the row's columns, so a field lives one level down.
fn subscriber_view(record: &json::Json) -> json::Json {
    let data = record.get("data");
    let field = |name: &str| {
        data.and_then(|data| data.get(name))
            .and_then(json::Json::as_str)
            .unwrap_or_default()
            .to_string()
    };
    json::Json::obj()
        .set(
            "id",
            json::Json::str(
                record
                    .get("id")
                    .and_then(json::Json::as_str)
                    .unwrap_or_default(),
            ),
        )
        .set("email", json::Json::str(&field("email")))
        .set("created_at", json::Json::str(&field("created_at")))
}

/// A stable id for `email`: 64-bit FNV-1a, hex.
///
/// Hand-rolled because a block has no `uuid` crate. Deriving the id from the
/// address rather than from a counter means the same address always lands on
/// the same row, so a retried subscribe cannot create a second one even if
/// the duplicate check above raced.
fn subscriber_id(email: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in email.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A refusal the caller can act on.
fn refuse(status: u16, message: &str) -> Response {
    Response::json(
        status,
        &json::Json::obj().set("error", json::Json::str(message)),
    )
}

/// A failure of the database, logged for the operator and generic for the
/// caller — a host error's message can name internals a public endpoint
/// should not repeat.
fn unavailable(error: HostError) -> Response {
    log::error(&format!("site/newsletter: {error}"));
    refuse(503, "the newsletter is temporarily unavailable")
}

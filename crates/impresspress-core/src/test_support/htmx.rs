//! Fire every htmx control a page emits, the way htmx fires it, and hold the
//! answer to what htmx does with it.
//!
//! Two halves of one contract live in different places. The page decides the
//! request: a `<form hx-post>` is submitted as
//! `application/x-www-form-urlencoded` with the fields a browser serializes
//! (no encoding extension ships with the chrome), a bare `hx-post` /
//! `hx-delete` button sends no fields, and htmx 2 puts a `DELETE`'s fields in
//! the query string rather than the body (`methodsThatUseUrlParams` is
//! `["get", "delete"]`). The handler decides the answer, and htmx swaps a 2xx
//! answer into the page unless the control says `hx-swap="none"` — the
//! chrome registers no `htmx:beforeSwap` filter — so an answer that is not
//! HTML is inserted as its own source text. Neither half can see the other:
//! a handler that parses JSON only answers every form submit 400, and one
//! that answers JSON replaces the row the operator was looking at with
//! `{"deleted": true}`. Both have shipped.
//!
//! [`fire_every_control`] closes the gap for one block's pages. It renders
//! every page [`crawl`] reaches from a [`Fixture`] — its pages, and every tab
//! and view they link to — finds every element carrying
//! `hx-post`/`hx-put`/`hx-patch`/`hx-delete`, builds the request htmx would
//! send — a form's fields serialized the way a browser serializes them, plus
//! what the operator types into the fields the page leaves empty — and
//! dispatches it through the real `handle()` of whichever block in the
//! fixture's [`Site`] declares that route. The answer must be a 2xx, and HTML
//! wherever it is swapped. `crate::htmx_guard` runs it over every block.
//!
//! A source scan cannot do this job. Half the controls build their URL at
//! render time (`hx-post={"/b/admin/users/" (id) "/disable"}`,
//! `hx-post=(restore_url)`), so the literal a scan would pair with a route
//! table is not in the source; and the handler a route names delegates its
//! parsing (`ops::*`, `settings_form::save_settings`,
//! `contracts::…::from_form`), so what it accepts is not visible at the
//! dispatch arm either. Rendering the page and posting its bytes reads both
//! halves from the only place they are both true.

use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};

use wafer_run::{Block, InputStream, Message, OutputStream};

use crate::endpoint_match::{action_for_method, match_template};

/// htmx attribute → the action the request arrives as.
const MUTATING_ATTRS: &[(&str, &str)] = &[
    ("hx-post=\"", "create"),
    ("hx-put=\"", "update"),
    ("hx-patch=\"", "update"),
    ("hx-delete=\"", "delete"),
];

/// One element carrying a mutating htmx attribute.
#[derive(Debug)]
pub struct Control {
    /// The action the request arrives as (`create` for `hx-post`, …).
    pub action: &'static str,
    /// The attribute's URL, unescaped.
    pub url: String,
    /// The element's start tag, for failure messages.
    pub tag: String,
    /// `hx-swap`, when the element declares one.
    pub swap: Option<String>,
    /// The form's inner HTML, when the element is a `<form>`.
    pub form: Option<String>,
}

impl Control {
    /// Whether htmx inserts a 2xx answer into the page.
    pub fn swaps(&self) -> bool {
        self.swap.as_deref() != Some("none")
    }

    /// `(action, route template, element name, field names)` — what
    /// identifies this control across two renders of the same page over two
    /// fixtures, whose seeded ids (and so URLs) differ. The route is the
    /// template `site` matches the URL against, so the Enable button of one
    /// row and the Disable button of another are two controls, not one
    /// `create <button>` that either row's position could stand in for.
    fn signature(&self, site: &Site) -> String {
        let path = self.url.split('?').next().unwrap_or_default();
        let Some((_, route)) = site.owner(self.action, path) else {
            panic!(
                "{} reaches {} {path}, which no block in the fixture's site declares",
                self.tag, self.action
            );
        };
        let name = tag_name(&self.tag);
        let fields: Vec<String> = self
            .form
            .as_deref()
            .map(|inner| {
                serialize_form(inner)
                    .into_iter()
                    .map(|field| field.name)
                    .collect()
            })
            .unwrap_or_default();
        format!("{} {route} <{name}> {fields:?}", self.action)
    }
}

/// Every element in `html` that carries a mutating htmx attribute, in
/// document order.
pub fn mutating_controls(html: &str) -> Vec<Control> {
    let mut found: Vec<(usize, Control)> = Vec::new();
    for (attr, action) in MUTATING_ATTRS {
        for (pos, _) in html.match_indices(attr) {
            let start = html[..pos].rfind('<').expect("an attribute sits in a tag");
            let end = pos + html[pos..].find('>').expect("the tag is closed");
            let tag = &html[start..=end];
            let url = attr_value(tag, &attr[..attr.len() - 2]).expect("the attribute has a value");
            let form = (tag_name(tag) == "form").then(|| {
                let inner = &html[end + 1..];
                inner[..inner.find("</form>").expect("the form is closed")].to_string()
            });
            found.push((
                start,
                Control {
                    action,
                    url,
                    tag: tag.to_string(),
                    swap: attr_value(tag, "hx-swap"),
                    form,
                },
            ));
        }
    }
    found.sort_by_key(|(start, _)| *start);
    found.into_iter().map(|(_, control)| control).collect()
}

/// The element name of a start tag (`<form hx-post=…>` → `form`).
fn tag_name(tag: &str) -> &str {
    let name = &tag[1..];
    &name[..name
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(name.len())]
}

/// The unescaped value of `name="…"` in a start tag. Matched only after
/// whitespace, so `name` does not find `data-name`.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = tag[from..].find(&needle).map(|i| i + from) {
        if tag[..i].ends_with(char::is_whitespace) {
            let rest = &tag[i + needle.len()..];
            return Some(unescape(&rest[..rest.find('"')?]));
        }
        from = i + needle.len();
    }
    None
}

/// Whether a start tag carries the boolean attribute `name`.
fn has_bool_attr(tag: &str, name: &str) -> bool {
    tag.match_indices(name).any(|(i, _)| {
        tag[..i].ends_with(char::is_whitespace)
            && tag[i + name.len()..]
                .starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/' || c == '=')
    })
}

/// Undo maud's attribute and text escaping.
fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// One field a browser submits.
#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub value: String,
    /// The control carries `required`, so the browser refuses to submit it
    /// empty.
    pub required: bool,
}

/// A form's fields, serialized the way a browser builds its form data set.
///
/// The rules that decide what a submit carries: a `disabled` control is never
/// submitted; a checkbox or radio is submitted only when `checked`, as `on`
/// when it has no value; buttons are not fields; a `<select>` submits its
/// `selected` option, or else its first one that is not disabled, and
/// nothing when the chosen option is itself disabled (a "Select a block…"
/// placeholder); a `<textarea>` submits its text. Document order is kept,
/// since `parse_form_body` keeps the last value of a repeated name — the
/// hidden-then-checkbox pattern the variable modals rely on.
pub fn serialize_form(inner: &str) -> Vec<Field> {
    let mut starts: Vec<(usize, &str)> = ["<input", "<textarea", "<select"]
        .iter()
        .flat_map(|open| inner.match_indices(open).map(|(i, _)| (i, *open)))
        .collect();
    starts.sort_unstable();

    let mut fields = Vec::new();
    for (i, open) in starts {
        let tag = &inner[i..=i + inner[i..].find('>').expect("the tag is closed")];
        let Some(name) = attr_value(tag, "name") else {
            continue;
        };
        if has_bool_attr(tag, "disabled") {
            continue;
        }
        let required = has_bool_attr(tag, "required");
        let value = match open {
            "<input" => {
                let kind = attr_value(tag, "type").unwrap_or_else(|| "text".to_string());
                match kind.as_str() {
                    "submit" | "button" | "reset" | "image" | "file" => continue,
                    "checkbox" | "radio" if !has_bool_attr(tag, "checked") => continue,
                    "checkbox" | "radio" => {
                        attr_value(tag, "value").unwrap_or_else(|| "on".to_string())
                    }
                    _ => attr_value(tag, "value").unwrap_or_default(),
                }
            }
            "<textarea" => {
                let body = &inner[i + tag.len()..];
                unescape(&body[..body.find("</textarea>").expect("the textarea is closed")])
            }
            _ => {
                let body = &inner[i + tag.len()..];
                let body = &body[..body.find("</select>").expect("the select is closed")];
                let options: Vec<(&str, &str)> = body
                    .split("<option")
                    .skip(1)
                    .map(|option| {
                        let end = option.find('>').expect("the option tag is closed");
                        let text = &option[end + 1..];
                        (
                            &option[..=end],
                            &text[..text.find("</option>").unwrap_or(text.len())],
                        )
                    })
                    .collect();
                let chosen = options
                    .iter()
                    .find(|(tag, _)| has_bool_attr(tag, "selected"))
                    .or_else(|| {
                        options
                            .iter()
                            .find(|(tag, _)| !has_bool_attr(tag, "disabled"))
                    });
                match chosen {
                    Some((tag, _)) if has_bool_attr(tag, "disabled") => continue,
                    Some((tag, text)) => {
                        attr_value(&format!(" {tag}"), "value").unwrap_or_else(|| unescape(text))
                    }
                    None => continue,
                }
            }
        };
        fields.push(Field {
            name,
            value,
            required,
        });
    }
    fields
}

/// The `application/x-www-form-urlencoded` fields htmx sends for `control`:
/// none for a bare button, the form's fields otherwise — with `operator_input`
/// standing in for what the operator fills in.
///
/// Panics on a required field the page leaves empty and `operator_input` does
/// not name: a new form should say what an operator puts there, rather than
/// the guard posting it blank and testing the refusal instead of the form.
pub fn encoded_fields(control: &Control, operator_input: &[(&str, &str)]) -> String {
    let Some(inner) = control.form.as_deref() else {
        return String::new();
    };
    let mut fields = serialize_form(inner);
    for (name, typed) in operator_input {
        let mut named = fields.iter_mut().filter(|f| f.name == *name).peekable();
        if named.peek().is_none() && inner.contains(&format!(" name=\"{name}\"")) {
            // A field the browser does not submit as rendered — the disabled
            // placeholder option of a select — is submitted once the
            // operator picks something.
            fields.push(Field {
                name: name.to_string(),
                value: typed.to_string(),
                required: true,
            });
            continue;
        }
        for field in named {
            if field.value.is_empty() {
                field.value = typed.to_string();
            }
        }
    }
    if let Some(blank) = fields.iter().find(|f| f.required && f.value.is_empty()) {
        panic!(
            "{} leaves the required field `{}` empty; add what an operator types into it \
             to the block's operator input",
            control.tag, blank.name
        );
    }
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for field in &fields {
        body.append_pair(&field.name, &field.value);
    }
    body.finish()
}

/// A response, rendered the way the HTTP boundary renders it.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub content_type: String,
    /// The `Location` header, on a redirect.
    pub location: Option<String>,
    pub body: String,
}

/// Collect `out` the way the HTTP boundary does.
pub async fn answer(out: OutputStream) -> Answer {
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let header = |name: &str| {
        parts
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    Answer {
        status: parts.status,
        content_type: header("content-type").unwrap_or_default(),
        location: header("location"),
        body: String::from_utf8_lossy(&parts.body).into_owned(),
    }
}

/// The blocks a page's controls may reach. A request goes to the block whose
/// declared endpoints match its action and path; one that no block declares
/// is a failure, not a skip.
#[derive(Clone)]
pub struct Site(pub Vec<Arc<dyn Block>>);

impl Site {
    /// The block declaring `action path`, and the template it matched.
    pub fn owner(&self, action: &str, path: &str) -> Option<(Arc<dyn Block>, String)> {
        let mut candidates = vec![path.to_string()];
        if !path.ends_with('/') {
            candidates.push(format!("{path}/"));
        }
        for block in &self.0 {
            for endpoint in block.info().endpoints {
                if action_for_method(endpoint.method) == action
                    && candidates
                        .iter()
                        .any(|p| match_template(&endpoint.path, p).is_some())
                {
                    return Some((block.clone(), endpoint.path));
                }
            }
        }
        None
    }
}

/// One page, as a concrete request: its path and query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub path: String,
    pub query: Vec<(String, String)>,
}

impl Page {
    pub fn at(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            query: Vec::new(),
        }
    }

    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));
        self
    }

    /// The URL a browser requests for this page.
    pub fn url(&self) -> String {
        if self.query.is_empty() {
            return self.path.clone();
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &self.query {
            query.append_pair(name, value);
        }
        format!("{}?{}", self.path, query.finish())
    }

    /// What makes two pages the same request, whatever order their query
    /// parameters were written in.
    fn key(&self) -> (String, Vec<(String, String)>) {
        let mut query = self.query.clone();
        query.sort();
        (self.path.clone(), query)
    }
}

/// Everything one run of a block's guard needs: a seeded context, the blocks
/// its pages reach, who is asking, and the pages — concrete, because they name
/// the fixture's seeded ids.
pub struct Fixture {
    /// The deployment the pages run in. Each request runs in the frame of
    /// the block that serves it, as a routed request does.
    pub ctx: super::TestContext,
    pub site: Site,
    /// The request message a visitor of these pages sends for
    /// `(action, path)` — their identity and roles.
    pub caller: fn(&str, &str) -> Message,
    /// The pages the crawl starts from: every page reached by its route
    /// rather than by a tab link on another page, plus any filtered view (a
    /// query [`crawl`] does not follow) worth rendering on its own. The tabs
    /// and views a page links to are found by [`crawl`], not listed here.
    pub pages: Vec<Page>,
    /// `(row template, URL)`: where each non-page `GET` row that needs one
    /// is dispatched — its path parameters filled with ids and names the
    /// fixture seeded, plus any query it requires — so it answers its
    /// success path rather than a not-found.
    pub probes: Vec<(&'static str, String)>,
    /// What an operator types into the fields the pages render empty. Every
    /// entry is a value the page itself would accept.
    pub operator_input: &'static [(&'static str, &'static str)],
}

/// A fresh [`Fixture`], built on demand — every control is fired against its
/// own, since each is a mutation that may change what the next one meets.
pub type MakeFixture = fn() -> Pin<Box<dyn Future<Output = Fixture>>>;

/// Dispatch `(action, url)` through `site` as `caller`, the way htmx sends it:
/// `HX-Request: true`, a form content type, and `fields` in the query string
/// for `GET`/`DELETE` and in the body otherwise.
pub async fn send(
    fixture: &Fixture,
    action: &str,
    url: &str,
    fields: &str,
    hx: bool,
) -> (String, Answer) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let (body, query) = if matches!(action, "retrieve" | "delete") {
        let query = [query, fields]
            .into_iter()
            .filter(|q| !q.is_empty())
            .collect::<Vec<_>>()
            .join("&");
        (String::new(), query)
    } else {
        (fields.to_string(), query.to_string())
    };
    let mut msg = (fixture.caller)(action, path);
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        msg.set_meta(format!("req.query.{name}"), value.as_ref());
    }
    if hx {
        msg.set_meta("http.header.hx-request", "true");
        // htmx sets this on every non-GET request it makes, fields or not.
        if action != "retrieve" {
            for key in [
                "http.header.content-type",
                "http.content_type",
                "req.content_type",
            ] {
                msg.set_meta(key, "application/x-www-form-urlencoded");
            }
        }
    }
    let Some((block, template)) = fixture.site.owner(action, path) else {
        panic!("no block in the fixture's site declares {action} {path}");
    };
    let out = block
        .handle(
            &fixture.ctx.clone().running_as(&block.info().name),
            msg,
            InputStream::from_bytes(body.into_bytes()),
        )
        .await;
    (template, answer(out).await)
}

/// Render `page` as a browser navigating to it would, and require a 200 HTML
/// document: a page that fails to render has no controls to check, and would
/// pass for that reason.
pub async fn render(fixture: &Fixture, page: &Page) -> String {
    let (_, answer) = send(fixture, "retrieve", &page.url(), "", false).await;
    assert_eq!(
        answer.status, 200,
        "{page:?} did not render: {}",
        answer.body
    );
    assert!(
        answer.content_type.starts_with("text/html"),
        "{page:?} is not an HTML page ({}); it does not belong in the page list",
        answer.content_type
    );
    answer.body
}

/// The query parameters that select a view of a page — a tab, a sub-tab, a
/// deleted-records view — rather than filter or page through its rows.
/// [`crawl`] follows a link that changes only these.
pub const VIEW_PARAMS: &[&str] = &["tab", "subtab", "view"];

/// A page [`crawl`] reached: one of the fixture's pages, or that page's path
/// under the view query a link on a reached page carries. Held as an index
/// rather than a [`Page`] because a fixture's pages name its seeded ids, and
/// every control is fired against a fresh fixture whose ids differ.
#[derive(Debug, Clone)]
pub struct Reached {
    base: usize,
    view: Option<Vec<(String, String)>>,
}

impl Reached {
    /// The concrete page over `fixture`.
    pub fn page(&self, fixture: &Fixture) -> Page {
        let base = &fixture.pages[self.base];
        match &self.view {
            None => base.clone(),
            Some(query) => Page {
                path: base.path.clone(),
                query: query.clone(),
            },
        }
    }
}

/// Every view of its own path that `html` links to: the query of each
/// `href`/`hx-get` whose path is `path` and whose parameters are all
/// [`VIEW_PARAMS`], in document order.
fn view_links(html: &str, path: &str) -> Vec<Vec<(String, String)>> {
    let mut found: Vec<(usize, Vec<(String, String)>)> = Vec::new();
    for attr in ["href=\"", "hx-get=\""] {
        for (pos, _) in html.match_indices(attr) {
            if !html[..pos].ends_with(char::is_whitespace) {
                continue;
            }
            let rest = &html[pos + attr.len()..];
            let Some(end) = rest.find('"') else {
                continue;
            };
            let url = unescape(&rest[..end]);
            let Some((link_path, query)) = url.split_once('?') else {
                continue;
            };
            if link_path != path {
                continue;
            }
            let query: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect();
            if !query.is_empty()
                && query
                    .iter()
                    .all(|(name, _)| VIEW_PARAMS.contains(&name.as_str()))
            {
                found.push((pos, query));
            }
        }
    }
    found.sort_by_key(|(pos, _)| *pos);
    found.into_iter().map(|(_, query)| query).collect()
}

/// Every page the fixture's pages lead to: each page, and every view of its
/// own path a reached page links to, followed until no new view appears.
/// A tab is rendered because a page links to it, not because a list names
/// it, so a new tab is covered the day it is linked. Breadth-first in
/// document order, so two crawls of one fixture reach the same pages in the
/// same order.
pub async fn crawl(fixture: &Fixture) -> Vec<Reached> {
    let mut reached: Vec<Reached> = (0..fixture.pages.len())
        .map(|base| Reached { base, view: None })
        .collect();
    let mut seen: BTreeSet<(String, Vec<(String, String)>)> =
        fixture.pages.iter().map(Page::key).collect();
    let mut next = 0;
    while next < reached.len() {
        let base = reached[next].base;
        let page = reached[next].page(fixture);
        for query in view_links(&render(fixture, &page).await, &page.path) {
            let view = Reached {
                base,
                view: Some(query),
            };
            if seen.insert(view.page(fixture).key()) {
                reached.push(view);
            }
        }
        next += 1;
    }
    reached
}

/// Fire every mutating htmx control on every page [`crawl`] reaches from the
/// pages `make` builds, each against its own fresh fixture, and require a 2xx
/// that is HTML wherever it swaps. Returns `"{action} {template}"` for every
/// route a control reached, so a caller can pin the ones its pages must keep
/// rendering.
pub async fn fire_every_control(make: MakeFixture) -> BTreeSet<String> {
    let mut fired = BTreeSet::new();
    let reached = crawl(&make().await).await;
    for view in &reached {
        let signatures: Vec<String> = {
            let fixture = make().await;
            let page = view.page(&fixture);
            mutating_controls(&render(&fixture, &page).await)
                .iter()
                .map(|control| control.signature(&fixture.site))
                .collect()
        };
        for (index, signature) in signatures.iter().enumerate() {
            let fixture = make().await;
            let page = view.page(&fixture);
            let controls = mutating_controls(&render(&fixture, &page).await);
            let control = &controls[index];
            assert_eq!(
                &control.signature(&fixture.site),
                signature,
                "{page:?} rendered its controls in a different order over a second fixture"
            );

            let fields = encoded_fields(control, fixture.operator_input);
            let (template, answer) =
                send(&fixture, control.action, &control.url, &fields, true).await;
            assert!(
                (200..300).contains(&answer.status),
                "{page:?} emits {}, whose request is answered {}: {}",
                control.tag,
                answer.status,
                answer.body
            );
            if control.swaps() {
                assert!(
                    answer.content_type.starts_with("text/html"),
                    "{page:?} emits {}, which swaps its answer into the page, and the \
                     answer is {} — htmx inserts it as text: {}",
                    control.tag,
                    answer.content_type,
                    answer.body
                );
            }
            fired.insert(format!("{} {template}", control.action));
        }
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two controls a page renders at the same position over two fixtures are
    /// the same control only if they reach the same route. The users tab's
    /// rows each carry a bare `hx-post` button — Disable for an active user,
    /// Enable for a disabled one — so a signature without the route let a
    /// second render that listed the rows the other way round pass as
    /// identical, and the guard fired Disable twice and Enable never.
    #[test]
    fn a_control_is_known_by_the_route_it_reaches() {
        let site = Site(vec![Arc::new(crate::blocks::admin::AdminBlock::new())]);
        let controls = mutating_controls(
            r#"<button hx-post="/b/admin/users/u1/disable">Disable</button>
               <button hx-post="/b/admin/users/u2/enable">Enable</button>
               <button hx-post="/b/admin/users/u3/disable">Disable</button>"#,
        );
        let [disable, enable, other_disable] = &controls[..] else {
            panic!("three controls: {controls:?}");
        };
        assert_ne!(
            disable.signature(&site),
            enable.signature(&site),
            "Disable and Enable reach different routes"
        );
        assert_eq!(
            disable.signature(&site),
            other_disable.signature(&site),
            "the same route on another row's id is the same control"
        );
    }

    /// The crawl follows a link to its own path that changes only the view
    /// — from `href` or `hx-get`, `&amp;`-escaped or not — and nothing that
    /// filters or pages, reaches another path, or is not a link attribute.
    #[test]
    fn a_page_links_to_its_views_by_tab_subtab_and_view() {
        let html = r#"
            <a class="tab" href="/b/admin/blocks?tab=services" hx-get="/b/admin/blocks?tab=services">S</a>
            <a href="/b/admin/blocks?tab=custom&amp;subtab=x">C</a>
            <a href="/b/admin/blocks?view=deleted">D</a>
            <a href="/b/admin/blocks?page=2">next</a>
            <a href="/b/admin/blocks?tab=services&amp;runtime=wasm">filtered</a>
            <a href="/b/admin/blocks">default</a>
            <a href="/b/admin/users?tab=roles">elsewhere</a>
            <div data-href="/b/admin/blocks?tab=hidden"></div>
        "#;
        let owned = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            view_links(html, "/b/admin/blocks"),
            vec![
                owned(&[("tab", "services")]),
                owned(&[("tab", "services")]),
                owned(&[("tab", "custom"), ("subtab", "x")]),
                owned(&[("view", "deleted")]),
            ]
        );
    }

    /// The serializer submits what a browser submits, and nothing a browser
    /// leaves out.
    #[test]
    fn a_form_serializes_the_way_a_browser_submits_it() {
        let inner = r#"
            <input type="text" name="key" required>
            <input type="text" name="value" value="a &amp; b">
            <input type="text" name="locked" value="x" disabled>
            <input type="hidden" name="sensitive" value="0">
            <input type="checkbox" name="sensitive" value="1" checked>
            <input type="checkbox" name="write" value="on">
            <input type="checkbox" name="bare" checked>
            <select name="grantee" required><option value="" disabled selected>Pick</option><option value="*">All</option></select>
            <select name="kind"><option value="db">DB</option><option value="config" selected>Config</option></select>
            <select name="first"><option>Plain text</option></select>
            <textarea name="query">SELECT 1 &lt; 2;</textarea>
            <button type="submit" name="go">Go</button>
        "#;
        let fields: Vec<(String, String, bool)> = serialize_form(inner)
            .into_iter()
            .map(|f| (f.name, f.value, f.required))
            .collect();
        let expect = |name: &str, value: &str, required: bool| {
            (name.to_string(), value.to_string(), required)
        };
        assert_eq!(
            fields,
            vec![
                expect("key", "", true),
                expect("value", "a & b", false),
                expect("sensitive", "0", false),
                expect("sensitive", "1", false),
                expect("bare", "on", false),
                expect("kind", "config", false),
                expect("first", "Plain text", false),
                expect("query", "SELECT 1 < 2;", false),
            ]
        );
    }
}

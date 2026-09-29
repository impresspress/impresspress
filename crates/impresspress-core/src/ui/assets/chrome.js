// The shared admin/portal chrome's browser behaviour, in load order.
//
// One file, one `<script src>`, one manifest hash. These four sections used
// to be four Rust raw strings inlined into the bottom of every rendered page
// (`ui::assets::{palette_js, drawer_js, toast_js, modal_js}`) -- 196 lines
// re-sent uncached on every request, and unreachable to a linter, a
// formatter or a source map. They are concatenated here in exactly the order
// the page used to emit them, so behaviour is unchanged:
//
//   1. command palette  (was emitted by `ui::Page::render`)
//   2. mobile drawer    (was emitted by `ui::Page::render`)
//   3. toasts           (was emitted by `ui::layout::page`)
//   4. modals           (was emitted by `ui::layout::page`)
//   5. htmx after-success effects (never inline; appended after section 4)
//
// Sections 1, 2, 4 and 5 are IIFEs with their own idempotence guards. Section 3's
// `showToast` listener is deliberately NOT wrapped: it binds `document.body`
// directly and declares nothing. The htmx error listeners that follow it ARE
// wrapped, because the three of them share one `toast()` helper and a shared
// helper at the top level of this file would be a global. Section 4 was
// unwrapped too until the pages
// stopped calling `openModal`/`closeModal` from `onclick` attributes; it now
// owns the shared delegated-action listener and exposes no globals. The rule
// and the vocabulary are documented at the head of that section.
//
// The page loads this with `defer` from `<head>`, so the whole file runs
// after parsing and every element these sections look for already exists --
// the same guarantee their old end-of-body placement gave them.
//
// What `defer` does NOT preserve is *when* that happens. The four inline
// scripts ran during parse, in the same document, with no network. This one
// waits on a fetch, and on a cold cache -- or on an `embed-assets`-off build
// streaming it from object storage -- that fetch is a real round trip.
// Nothing in here paints, so the delay is mostly invisible -- the palette
// trigger, the drawer control and the modal handlers are simply inert until
// it lands, where before they worked as soon as the parser passed them --
// with one visible exception: the palette's platform swap in section 1
// rewrites a server-rendered glyph. See the note there.

// --- 1. command palette ---
(function () {
  if (window.__cmdkInit) return;
  window.__cmdkInit = true;
  const el = document.getElementById('cmdk');
  if (!el) return;
  const input = document.getElementById('cmdk-input');
  const list = document.getElementById('cmdk-list');

  const items = () => Array.from(list.querySelectorAll('.palette__item'));
  let selected = 0;

  function open() {
    el.dataset.open = 'true';
    el.setAttribute('aria-hidden', 'false');
    input.value = '';
    apply('');
    requestAnimationFrame(() => input.focus());
  }
  function close() {
    el.dataset.open = 'false';
    el.setAttribute('aria-hidden', 'true');
  }
  function visibleItems() { return items().filter(i => !i.classList.contains('is-hidden')); }

  function apply(query) {
    const q = query.trim().toLowerCase();
    items().forEach(i => {
      const k = (i.dataset.keywords || '').toLowerCase();
      const match = !q || k.includes(q);
      i.classList.toggle('is-hidden', !match);
      i.setAttribute('aria-selected', 'false');
    });
    const vis = visibleItems();
    selected = 0;
    if (vis[0]) vis[0].setAttribute('aria-selected', 'true');
  }

  function move(delta) {
    const vis = visibleItems();
    if (!vis.length) return;
    vis[selected]?.setAttribute('aria-selected', 'false');
    selected = (selected + delta + vis.length) % vis.length;
    vis[selected].setAttribute('aria-selected', 'true');
    vis[selected].scrollIntoView({ block: 'nearest' });
  }

  function activate() {
    const vis = visibleItems();
    const sel = vis[selected];
    if (!sel?.dataset.href) return;
    if (sel.dataset.external === 'true') {
      window.open(sel.dataset.href, '_blank', 'noopener,noreferrer');
    } else {
      window.location.assign(sel.dataset.href);
    }
  }

  // Hotkeys
  document.addEventListener('keydown', (e) => {
    const isMod = e.metaKey || e.ctrlKey;
    if (isMod && e.key.toLowerCase() === 'k') { e.preventDefault(); open(); return; }
    if (el.dataset.open !== 'true') return;
    if (e.key === 'Escape') { e.preventDefault(); close(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
    else if (e.key === 'Enter') { e.preventDefault(); activate(); }
  });

  // Click triggers
  document.addEventListener('click', (e) => {
    const t = e.target.closest('[data-action]');
    if (!t) return;
    if (t.dataset.action === 'palette-open') { e.preventDefault(); open(); }
    if (t.dataset.action === 'palette-close') { e.preventDefault(); close(); }
  });

  // The shortcut hint defaults to the Mac glyph; swap to Ctrl elsewhere so
  // the advertised key matches what the keydown handler above accepts.
  //
  // This is the one thing in this file a visitor can see happen. It used to
  // run during parse; it now waits on this file's fetch, so a non-Mac visitor
  // on a cold cache sees the server-rendered `⌘` painted and then replaced.
  // The string grows from one glyph to four characters, so on a narrow
  // viewport the topbar re-lays out rather than merely re-texting. Rendering
  // the label server-side would trade that for either a wrong glyph on Mac or
  // a changed rendered output on every shelled page; the flash is on a
  // once-per-deploy cold cache only, and the button works throughout, so it
  // is documented rather than designed away. If it ever needs to go, the fix
  // is a platform-neutral server-rendered label, not an inline script.
  if (!/Mac|iPhone|iPad|iPod/.test(navigator.platform || '')) {
    document.querySelectorAll('.topbar__palette-cmd').forEach((n) => { n.textContent = 'Ctrl'; });
    document.querySelectorAll('.shell__palette-icon').forEach((n) => { n.textContent = 'Ctrl K'; });
  }

  // Linked table rows (`.data-table__row--linked`) style as clickable; make
  // the whole row actually navigate via its row-href anchor, unless the
  // click landed on an interactive element of its own.
  document.addEventListener('click', (e) => {
    const row = e.target.closest('.data-table__row--linked');
    if (!row || e.target.closest('a, button, input, select, label, textarea')) return;
    const anchor = row.querySelector('.data-table__row-href a');
    if (anchor) anchor.click();
  });

  // Item click → navigate
  list.addEventListener('click', (e) => {
    const item = e.target.closest('.palette__item');
    if (!item?.dataset.href) return;
    if (item.dataset.external === 'true') {
      window.open(item.dataset.href, '_blank', 'noopener,noreferrer');
    } else {
      window.location.assign(item.dataset.href);
    }
  });

  input.addEventListener('input', (e) => apply(e.target.value));

  // Keyboard scrolling for the app shell. The document never scrolls (the
  // .shell grid is 100vh; .shell__body is the real scroller), so with no
  // focused element PageDown/PageUp/Space/Home/End/arrows would silently do
  // nothing. Registered after the palette handler above, so an open palette
  // (which preventDefaults its own keys) wins. Only fires when the event
  // target is the page itself — typing in fields and focused widgets keep
  // their native behavior.
  document.addEventListener('keydown', (e) => {
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target !== document.body && e.target !== document.documentElement) return;
    const scroller = document.querySelector('.shell__body');
    if (!scroller) return;
    const pageStep = scroller.clientHeight * 0.9;
    const lineStep = 40;
    let dy;
    switch (e.key) {
      case 'PageDown': dy = pageStep; break;
      case 'PageUp': dy = -pageStep; break;
      case ' ': dy = e.shiftKey ? -pageStep : pageStep; break;
      case 'ArrowDown': dy = lineStep; break;
      case 'ArrowUp': dy = -lineStep; break;
      case 'Home': scroller.scrollTo({ top: 0 }); e.preventDefault(); return;
      case 'End': scroller.scrollTo({ top: scroller.scrollHeight }); e.preventDefault(); return;
      default: return;
    }
    scroller.scrollBy({ top: dy });
    e.preventDefault();
  });
})();

// --- 2. mobile sidebar drawer ---
(function () {
  if (window.__drawerInit) return;
  window.__drawerInit = true;
  var body = document.body;
  function open() { body.setAttribute('data-drawer-open', 'true'); }
  function close() { body.removeAttribute('data-drawer-open'); }
  document.addEventListener('click', function (e) {
    var t = e.target;
    if (!(t instanceof Element)) return;
    var actEl = t.closest('[data-action]');
    var action = actEl ? actEl.getAttribute('data-action') : null;
    if (action === 'drawer-open') { open(); e.preventDefault(); return; }
    if (action === 'drawer-close') { close(); e.preventDefault(); return; }
    if (body.hasAttribute('data-drawer-open') && t.closest('.sidebar a')) {
      close();
    }
  });
  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape' && body.hasAttribute('data-drawer-open')) {
      close();
    }
  });
})();

// --- 3. toast notifications (htmx HX-Trigger channel) ---
document.body.addEventListener("showToast", function(e) {
    var d = e.detail || {};
    var c = document.getElementById("toast-container");
    if (!c) return;
    var t = document.createElement("div");
    var kind = ["success", "error", "warning", "info"].indexOf(d.type) >= 0 ? d.type : "info";
    t.className = "toast toast-" + kind;
    var message = document.createElement("span");
    message.textContent = String(d.message || "");
    var dismiss = document.createElement("button");
    dismiss.className = "toast-dismiss";
    dismiss.type = "button";
    dismiss.setAttribute("aria-label", "Dismiss");
    dismiss.textContent = "×";
    dismiss.addEventListener("click", function() { t.remove(); });
    t.appendChild(message);
    t.appendChild(dismiss);
    c.appendChild(t);
    setTimeout(function() { t.remove(); }, 4000);
});

// A refused htmx request must not be SILENT.
//
// htmx 2.0.4's default `responseHandling` is
// `[{code:"204",swap:false},{code:"[23]..",swap:true},{code:"[45]..",swap:false,error:true}]`:
// a 4xx/5xx is deliberately not swapped, and htmx raises `htmx:responseError`
// instead. Nothing in this tree listened for it, so every refusal on every
// shelled page rendered as nothing happening at all — no swap, no message, the
// modal sitting open unchanged. To the operator that is indistinguishable from
// the request having worked, or from the 500 a refusal used to be, which is
// exactly what the 2026-09-10 audit of the live server was about: a duplicate
// variable key now answers a correct 409, and without this listener the correct
// 409 would look the same as the wrong 500 did.
//
// One listener, not one per form. Invisible 4xx is a property of the whole
// admin SSR surface, so the fix belongs on the shared channel rather than in an
// `hx-on--after-request` attribute copied onto each control — which is what
// `blocks/products/pages.rs` had to do before this existed.
//
// The body is the `{"error": "<Code>", "message": "<text>"}` envelope
// `wafer_block::http_codec` renders for every error terminal, and `message` is
// the sentence written for the operator. Everything else is a fallback, ending
// in one that at least names the status: an empty toast is the same silence
// this listener exists to remove.
//
// `htmx:responseError` covers only the half where a response ARRIVED — in
// htmx 2.0.4 it is fired from `handleAjaxResponse` and nowhere else. A request
// that never reached the server takes a different path entirely: `xhr.onerror`
// fires `htmx:sendError` and `xhr.ontimeout` fires `htmx:timeout`, each after an
// `htmx:afterRequest` whose `detail.successful` was never assigned (it is set
// only inside `handleAjaxResponse`). Both are covered below, with their own
// wording — there is no status and no body to report, and "request failed"
// would not tell the operator the thing that matters, which is that nothing was
// sent and retrying is the right move.
//
// `htmx:sendAbort` is deliberately NOT covered. An abort is the page's own
// doing — `hx-sync` superseding an in-flight request, a navigation away — so
// toasting it would manufacture noise on exactly the pages that abort most,
// and nothing in this tree aborts a request a person is waiting on.
//
// One page can issue MANY requests without a person asking for any of them,
// and when they all fail they all fail the same way. `blocks/llm/ui.rs` renders
// a status badge per model with `hx-trigger="load"`, so an admin opening that
// page with twenty configured models and an unreachable backend would get
// twenty identical toasts stacked, each sitting for four seconds.
//
// So an AUTO-TRIGGERED repeat collapses, and a repeat a PERSON asked for never
// does. Those are two different facts. Twenty badges loading themselves and
// failing is one thing that went wrong, reported twenty times; an operator who
// clicks a button, reads the toast, and clicks again is telling the page they
// want another answer, and suppressing that is the "nothing happened" silence
// this listener exists to remove.
//
// `detail.requestConfig.triggeringEvent` is the discriminator, and `isTrusted`
// is what makes it readable without guessing at htmx's internals: only the
// browser sets `isTrusted` on an event it dispatched because of a real user
// action. `hx-trigger="load"` and `hx-trigger` polling issue their request with
// no event at all (htmx's `loadImmediately`/`doPoll` call the issuer with just
// the element), and htmx's own synthetic triggers — `intersect` for
// `hx-trigger="revealed"`, `hx:poll:trigger` — carry an event it constructed,
// which is untrusted. A missing `requestConfig` means htmx did not say, and
// "could not tell" must not become "suppress": it shows.
//
// The key is NOT the message text. Every 500 in the tree is
// `wafer_block::response::err_internal`, which mints a fresh 8-byte correlation
// id per call and renders `Internal server error (ref: <hex>)` — so those
// twenty badges produce twenty DIFFERENT strings and text-keyed suppression
// does nothing for the exact case it was written for. The key is the status,
// the envelope's `error` code, and the message with its trailing `(ref: …)`
// removed; the ref still appears in the toast, because it is what an operator
// quotes into a support ticket. `blocks/errors.rs`'s
// `two_internal_errors_differ_only_by_the_correlation_ref` pins that shape from
// a real rendered response.
//
// The window is a sliding one: an auto-triggered repeat resets it, so a page
// polling a broken endpoint every two seconds toasts once and then stays quiet
// while it goes on failing, rather than re-toasting forever. It is a little
// longer than the toast's own four-second dismissal, so a duplicate cannot
// arrive just as its twin disappears and read as a second, separate failure.
//
// `data-error-label` names the CONTROL. A generic "Request failed (502)" tells
// an operator nothing about which of the Archive and Deactivate buttons on
// `blocks/products/pages.rs`'s payment-link rows just failed, or whether
// anything was written — which is what the per-button `hx-on--after-request`
// this listener replaced used to say. A control that carries the attribute
// lends its sentence to every failure it causes; the server's own message still
// wins when there is one, because it is more specific than either.
//
// Wrapped, unlike the `showToast` listener above it, because the three
// listeners share these helpers and the suppression state, and none of them may
// become a global.
(function () {
    if (window.__htmxErrorToastInit) return;
    window.__htmxErrorToastInit = true;

    var DEDUPE_WINDOW_MS = 5000;
    var lastShownAt = new Map();

    // Whether a person asked for this request. See the note above on
    // `isTrusted`; "could not tell" answers true, because silence is the
    // failure mode worth avoiding.
    function userInitiated(detail) {
        var config = detail && detail.requestConfig;
        if (!config) return true;
        var event = config.triggeringEvent;
        return !!event && event.isTrusted === true;
    }

    // The element that made the request, for `data-error-label`. htmx puts it
    // on the request config; the bubbled event's target is the same element and
    // is the fallback for a detail that carries no config.
    function label(e) {
        var config = e.detail && e.detail.requestConfig;
        var element = (config && config.elt) || e.target;
        if (!element || typeof element.closest !== "function") return "";
        var labelled = element.closest("[data-error-label]");
        return labelled ? labelled.getAttribute("data-error-label") || "" : "";
    }

    // `Internal server error (ref: a1b2…)` and the next one differ only in the
    // ref, and for suppression they are the same failure.
    function withoutRef(message) {
        return message.replace(/\s*\(ref:[^)]*\)\s*$/, "");
    }

    function show(message, key, fromUser) {
        var now = Date.now();
        // Forget anything past the window first, so a long-lived page cannot
        // accumulate one entry per distinct failure it has ever seen.
        lastShownAt.forEach(function (at, seen) {
            if (now - at > DEDUPE_WINDOW_MS) lastShownAt.delete(seen);
        });
        var suppressed = !fromUser && lastShownAt.has(key);
        // Recorded either way: a toast a person asked for still starts the
        // window, so the auto-triggered repeats behind it stay quiet.
        lastShownAt.set(key, now);
        if (suppressed) return;
        document.body.dispatchEvent(new CustomEvent("showToast", {
            detail: { type: "error", message: message }
        }));
    }

    document.body.addEventListener("htmx:responseError", function(e) {
        var xhr = (e.detail && e.detail.xhr) || {};
        var text = typeof xhr.responseText === "string" ? xhr.responseText : "";
        var status = xhr.status || 0;
        var message = "";
        var code = "";
        // Parsed only when the body LOOKS like that envelope. A refusal
        // rendered as an HTML error page is also a 4xx, and putting a whole
        // document through `textContent` into a toast is worse than not
        // toasting at all.
        if (text.replace(/^\s+/, "").charAt(0) === "{") {
            try {
                var body = JSON.parse(text);
                if (body && typeof body.message === "string") { message = body.message; }
                if (body && typeof body.error === "string") { code = body.error; }
            } catch (err) { /* not the envelope after all; fall through */ }
        }
        if (!message) {
            // No usable message: name the control if it named itself, and the
            // status either way, so the operator has both something to act on
            // and something to report.
            var control = label(e);
            var subject = control || "Request failed";
            message = status ? subject + " (" + status + ")" : subject;
        }
        show(message, status + "|" + code + "|" + withoutRef(message), userInitiated(e.detail));
    });

    document.body.addEventListener("htmx:sendError", function(e) {
        var control = label(e);
        show(
            control
                ? control + " — the server could not be reached."
                : "Could not reach the server. Check your connection and try again.",
            "sendError|" + control,
            userInitiated(e.detail)
        );
    });

    document.body.addEventListener("htmx:timeout", function(e) {
        var control = label(e);
        show(
            control
                ? control + " — the server did not answer in time."
                : "The server did not answer in time. Try again.",
            "timeout|" + control,
            userInitiated(e.detail)
        );
    });
})();

// --- 4. modals, and the shared delegated-action listener ---
//
// Everything a modal does lives in this one IIFE, so `openModal`/`closeModal`
// are no longer globals. They had to be, because pages spelled their controls
// as `onclick="openModal('create-role')"` attributes. Those are gone; the only
// other caller was the htmx `HX-Trigger` response-header channel, handled here.
//
// ## The delegated-action rule
//
// Page markup carries no `on*=` attribute. A control declares WHAT it does with
// `data-action="<verb>"` plus whatever `data-*` operands the verb needs, and a
// delegated listener on `document` reads them back. The reason is written out at
// `blocks/admin/pages/network.rs`: maud escapes an attribute VALUE as HTML, but
// an `onclick` value is not HTML, it is JavaScript source, so a page that ever
// interpolates request-shaped text into one has a script-execution site with no
// escaping in the way. A `data-*` operand read with `getAttribute` is inert text
// whatever it holds. It also collapses the same few behaviours — open a modal,
// close a modal, reveal a password field — from a hundred hand-written copies
// down to one.
//
// `data-action` is one namespace shared by every script in the tree, so a verb
// is prefixed by whoever owns it. The verbs below are chrome's; a block's own
// script owns verbs named for that block's page and ignores the rest. A listener
// that does not recognise a verb MUST fall through silently — more than one
// delegated listener sees every click.
//
// Two known exceptions to the prefix rule, both pre-existing and both safe only
// because of where they sit. `blocks/files/assets/files-browser.js` owns the
// bare verbs share/copy/delete/revoke/create/cancel, but binds them with
// `addEventListener` on the elements it built itself rather than reading them
// back from a delegated listener, so nothing here or there can collide.
// `blocks/tickets/public.rs` puts `data-action="ticket_submit"` on a Cloudflare
// Turnstile widget — `data-action` is Turnstile's OWN attribute there, not a
// verb of ours; the page renders without the shell, so this file is not loaded
// on it. Give that page a shell and the two meanings meet.
//
// Chrome's verbs:
//   modal-open    + data-modal-target="<id>"   reveal that modal overlay
//   modal-close   + data-modal-target="<id>"   hide it (omit the operand to
//                                              close the enclosing overlay)
//   reveal-toggle + data-reveal-target="<id>"  swap a password field between
//                                              masked and plain, and swap the
//                                              button's label when it carries
//                                              data-reveal-show/-hide
//   mirror-value  + data-mirror-target="<id>"  on change, copy this control's
//                                              value into that field (the
//                                              colour swatch beside its hex box)
//   copy-text     + data-copy-source="<id>"    put that element's text on the
//                                              clipboard and flash "Copied" on
//                                              the button for 1.5s
//   drawer-open / drawer-close                 section 2 above
//
// Plus two attributes with no verb, because they describe the element rather
// than a control acting on it:
//   .modal-overlay[data-modal-dismiss]   a click on the backdrop closes it
//   [data-stop-propagation]              a click inside it reaches no ancestor
//                                        listener — the escape hatch for a link
//                                        nested in a clickable card
//   [data-submit-on-enter]               a textarea where Enter submits the
//                                        enclosing form and Shift+Enter keeps
//                                        inserting a newline (chat composers)
(function () {
    if (window.__modalInit) return;
    window.__modalInit = true;

    // `data-stop-propagation` reproduces exactly what the inline
    // `onclick="event.stopPropagation()"` it replaced did: a BUBBLE-phase
    // listener ON THE MARKED ELEMENT, so everything at or below that element
    // still fires and nothing above it does — the listener it exists to
    // silence (htmx's, bound on the enclosing card) is above it.
    //
    // That cannot be done by stopping the event in a document listener.
    // A bubbling one at `document` fires after the card, too late; a capturing
    // one at `document` fires before anything and aborts the WHOLE dispatch,
    // which would also silence the marked element's own listeners and every
    // other delegated behaviour inside it — a trap for the next element that
    // gets this attribute. So the capture pass only ARMS the real listener,
    // on the element, for this one dispatch, and it removes itself again.
    // Stopping propagation does not cancel the default action, so the link
    // still navigates.
    document.addEventListener("click", function (e) {
        var t = e.target;
        if (!(t instanceof Element)) return;
        var marked = t.closest("[data-stop-propagation]");
        if (!marked) return;
        marked.addEventListener("click", function stopOnce(inner) {
            marked.removeEventListener("click", stopOnce);
            inner.stopPropagation();
        });
    }, true);

    function openModal(id) {
        var m = document.getElementById(id);
        if (m) m.removeAttribute("hidden");
    }
    function closeModal(id) {
        var m = document.getElementById(id);
        if (m) m.setAttribute("hidden", "");
    }

    function revealToggle(btn) {
        var input = document.getElementById(btn.getAttribute("data-reveal-target") || "");
        if (!input) return;
        var masked = input.type === "password";
        input.type = masked ? "text" : "password";
        // A button with no label operands keeps the label it was rendered with
        // — the auth pages use one static "Toggle password visibility" for both
        // states, and did before this was delegated.
        var label = btn.getAttribute(masked ? "data-reveal-hide" : "data-reveal-show");
        if (label === null) return;
        btn.title = label;
        btn.setAttribute("aria-label", label + " value");
    }

    // The text is read out of the DOM rather than carried in the operand: a
    // secret that is only shown once should not also be written into an
    // attribute, and reading `innerText` needs no escaping at all.
    function copyText(btn) {
        var src = document.getElementById(btn.getAttribute("data-copy-source") || "");
        var text = src ? src.innerText : "";
        if (!text || !navigator.clipboard) return;
        navigator.clipboard.writeText(text).then(function () {
            var was = btn.textContent;
            btn.textContent = "Copied";
            setTimeout(function () { btn.textContent = was; }, 1500);
        });
    }

    document.addEventListener("click", function (e) {
        var t = e.target;
        if (!(t instanceof Element)) return;

        // Backdrop dismissal: only a click that landed on the overlay itself,
        // never one that bubbled out of the dialog inside it.
        if (t.matches(".modal-overlay[data-modal-dismiss]")) {
            closeModal(t.id);
            return;
        }

        var el = t.closest("[data-action]");
        if (!el) return;
        var action = el.getAttribute("data-action");
        if (action === "modal-open") {
            openModal(el.getAttribute("data-modal-target") || "");
            e.preventDefault();
        } else if (action === "modal-close") {
            var target = el.getAttribute("data-modal-target");
            if (target === null) {
                var overlay = el.closest(".modal-overlay");
                if (overlay) closeModal(overlay.id);
            } else {
                closeModal(target);
            }
            e.preventDefault();
        } else if (action === "reveal-toggle") {
            revealToggle(el);
            e.preventDefault();
        } else if (action === "copy-text") {
            copyText(el);
            e.preventDefault();
        }
    });

    document.addEventListener("change", function (e) {
        var el = e.target;
        if (!(el instanceof Element)) return;
        if (el.getAttribute("data-action") !== "mirror-value") return;
        var target = document.getElementById(el.getAttribute("data-mirror-target") || "");
        if (target) target.value = el.value;
    });

    document.addEventListener("keydown", function (e) {
        if (e.key === "Escape") {
            var m = document.querySelector(".modal-overlay:not([hidden])");
            if (m) m.setAttribute("hidden", "");
            return;
        }
        // Two chat composers had the same nine-word `onkeydown` attribute.
        if (e.key !== "Enter" || e.shiftKey) return;
        var box = e.target;
        if (!(box instanceof Element) || !box.hasAttribute("data-submit-on-enter")) return;
        var form = box.closest("form");
        if (!form) return;
        e.preventDefault();
        form.requestSubmit();
    });

    // The htmx response-header channel, both directions. A handler that
    // answers with a modal's contents says so in `HX-Trigger-After-Swap`
    // rather than appending a script that reveals the overlay itself — four
    // copies of that script existed, one of them built by `format!` with a
    // record id interpolated into JavaScript source.
    document.body.addEventListener("closeModal", function (e) {
        var d = e.detail || {};
        if (d.id) closeModal(d.id);
    });
    document.body.addEventListener("openModal", function (e) {
        var d = e.detail || {};
        if (d.id) openModal(d.id);
    });
})();

// --- 5. after-success effects of htmx requests ---
//
// What a control does once ITS OWN htmx request succeeds, declared as
// attributes on the element that issued the request (the form, or the button
// carrying `hx-post`/`hx-delete`):
//
//   data-reload-on-success          reload the page — for an `hx-swap="none"`
//                                   action whose result only a fresh render
//                                   shows (a created row, a restored product)
//   data-reset-on-success           reset the form, so the next entry starts
//                                   empty
//   data-remove-on-success="<id>"   remove that element — an empty-state line
//                                   the swap has just contradicted
//   data-scroll-on-success="<id>"   scroll that element to its bottom — the
//                                   list the swap has just appended to
//
// These are not `hx-on--after-request` attributes because htmx compiles an
// `hx-on` value with `new Function`, which a content-security policy treats as
// eval, and the policy every page is served with has no `'unsafe-eval'`:
// `wafer-run/security-headers` refuses to add it. An `hx-on` handler therefore
// never runs — the browser logs a CSP refusal and the page does nothing.
// `ui::layout::page` sets htmx's `allowEval` to false as well, so htmx itself
// refuses every eval-shaped attribute (`hx-on`, a `js:` value, a trigger
// filter) with an `htmx:evalDisallowedError` rather than leaving it to the
// policy, and `ui::tests::pages_carry_no_htmx_eval_attributes` keeps them out
// of the markup.
//
// Only the issuing element's own attributes count, read from
// `detail.elt`. A failed request does none of this; section 3 above toasts it.
(function () {
    if (window.__successEffectsInit) return;
    window.__successEffectsInit = true;

    document.body.addEventListener("htmx:afterRequest", function (e) {
        var detail = e.detail || {};
        if (detail.successful !== true) return;
        var el = detail.elt;
        if (!(el instanceof Element)) return;

        var removeId = el.getAttribute("data-remove-on-success");
        if (removeId) {
            var gone = document.getElementById(removeId);
            if (gone) gone.remove();
        }
        if (el.hasAttribute("data-reset-on-success") && typeof el.reset === "function") {
            el.reset();
        }
        var scrollId = el.getAttribute("data-scroll-on-success");
        if (scrollId) {
            var list = document.getElementById(scrollId);
            if (list) list.scrollTop = list.scrollHeight;
        }
        if (el.hasAttribute("data-reload-on-success")) {
            window.location.reload();
        }
    });
})();

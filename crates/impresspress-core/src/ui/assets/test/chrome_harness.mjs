// Shared harness for `chrome.js` unit tests (`node --test
// crates/impresspress-core/src/ui/assets/test/*.test.mjs`).
//
// `chrome.js` is a SCRIPT, not a tail: `ui/layout.rs` loads it with one
// `<script src>` and it declares nothing on `window` except the three
// idempotence flags its IIFEs use. So there is nothing to import, and this
// harness does what the browser does instead — run the file's source against a
// document — except the document is a stub small enough to assert on.
//
// Same `new Function` shape as `blocks/dev/assets/test/harness.mjs` and
// `./harness.mjs`, for the same reason written out there: `vm` would be the
// conventional tool, but the file's top level only registers listeners, so a
// plain factory closing over the stub globals is simpler and nothing here
// needs module semantics.
//
// The stub is deliberately partial. Sections 1 (command palette), 2 (drawer)
// and 4 (modals) are IIFEs that bail or merely bind a delegated listener when
// the elements they look for are absent, which is exactly what a stub with no
// `#cmdk` gives them — so loading the whole real file costs nothing and keeps
// the tests honest about the file as shipped, rather than about an extract of
// it. What the tests drive is section 3 (toasts and the htmx error listeners)
// and section 5 (htmx after-success effects).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const source = fs.readFileSync(path.join(here, '..', 'chrome.js'), 'utf8');

/** The smallest thing `dispatchEvent` can run listeners against. */
class StubEventTarget {
  constructor() {
    this._listeners = new Map();
  }

  addEventListener(type, listener) {
    if (!this._listeners.has(type)) this._listeners.set(type, []);
    this._listeners.get(type).push(listener);
  }

  removeEventListener(type, listener) {
    const list = this._listeners.get(type) || [];
    const at = list.indexOf(listener);
    if (at >= 0) list.splice(at, 1);
  }

  dispatchEvent(event) {
    event.target = this;
    for (const listener of (this._listeners.get(event.type) || []).slice()) {
      listener.call(this, event);
    }
    return true;
  }
}

/** `new CustomEvent(type, {detail})`, which is all `chrome.js` constructs. */
class StubCustomEvent {
  constructor(type, init) {
    this.type = type;
    this.detail = (init && init.detail) !== undefined ? init.detail : null;
  }
}

/**
 * The half of htmx's `responseInfo` the error listeners read.
 *
 * `requestConfig.triggeringEvent` is what separates a request a PERSON made
 * from one the page made for itself: htmx passes the DOM event through for a
 * click or a submit, and the browser marks those `isTrusted`; `hx-trigger="load"`
 * and polling issue their request with no event at all, and htmx's own
 * synthetic triggers carry one it constructed, which is untrusted.
 * `requestConfig.elt` is the element that made the request, which is where
 * `data-error-label` is read from.
 */
function responseDetail(xhr, { user = false, label = null, noConfig = false } = {}) {
  if (noConfig) return { xhr };
  const elt = fakeElement('button');
  if (label !== null) elt.setAttribute('data-error-label', label);
  return {
    xhr,
    requestConfig: {
      elt,
      triggeringEvent: user ? { type: 'click', isTrusted: true } : undefined
    }
  };
}

/** An element with only what the toast section touches. */
function fakeElement(tag) {
  return {
    tag,
    className: '',
    textContent: '',
    type: '',
    attributes: {},
    children: [],
    removed: false,
    setAttribute(name, value) {
      this.attributes[name] = value;
    },
    getAttribute(name) {
      return name in this.attributes ? this.attributes[name] : null;
    },
    appendChild(child) {
      this.children.push(child);
      return child;
    },
    addEventListener() {},
    remove() {
      this.removed = true;
    },
    // The stub has no tree, so the nearest labelled ancestor is the element
    // itself or nothing — which is how the real buttons carry the attribute.
    closest(selector) {
      const attribute = selector.replace(/^\[|\]$/g, '');
      return attribute in this.attributes ? this : null;
    }
  };
}

/**
 * An element section 5 can act on: the one that issued a request, or one it
 * names by id. It is what the sandbox's `Element` constructs, so the section's
 * `instanceof Element` check admits it, and it records what was done to it.
 */
class StubElement {
  constructor(attributes = {}) {
    this.attributes = { ...attributes };
    this.removed = false;
    this.resets = 0;
    this.scrollTop = 0;
    this.scrollHeight = 0;
  }

  getAttribute(name) {
    return name in this.attributes ? this.attributes[name] : null;
  }

  hasAttribute(name) {
    return name in this.attributes;
  }

  remove() {
    this.removed = true;
  }
}

/**
 * Load `chrome.js` against a fresh stub document.
 *
 * @param {object} [options]
 * @param {boolean} [options.toastContainer]  whether the page has a
 *   `#toast-container`. The shipped layout always renders one; `false` is the
 *   case the toast listener itself guards against, and a test that fires an
 *   error with no container is asserting that nothing throws.
 * @returns {{body: StubEventTarget, toasts: () => Array<{kind: string, text: string}>}}
 */
export function loadChrome({ toastContainer = true } = {}) {
  const container = toastContainer ? fakeElement('div') : null;
  // Elements a test places in the document by id, for section 5 to find.
  const byId = new Map();
  let reloads = 0;
  // A clock the test drives. The error listeners suppress a repeat of the same
  // message inside a time window, and "what happens once the window has passed"
  // is not a property a test can assert by waiting five real seconds.
  let clock = 1_000_000;
  const body = new StubEventTarget();
  Object.assign(body, {
    attributes: {},
    setAttribute(name, value) {
      this.attributes[name] = value;
    },
    removeAttribute(name) {
      delete this.attributes[name];
    },
    hasAttribute(name) {
      return name in this.attributes;
    }
  });

  const sandbox = {
    window: {
      location: {
        reload() {
          reloads += 1;
        }
      }
    },
    document: {
      body,
      getElementById: (id) => (id === 'toast-container' ? container : byId.get(id) || null),
      createElement: (tag) => fakeElement(tag),
      querySelector: () => null,
      querySelectorAll: () => [],
      addEventListener() {},
      documentElement: {}
    },
    // Section 2 narrows a click target with `instanceof Element` (nothing here
    // fires a click), and section 5 narrows the element that issued a request
    // the same way — which is the check `StubElement` exists to pass.
    Element: StubElement,
    // The toast's own 4s auto-dismiss. Tests read the container synchronously,
    // so the timer never needs to fire — but it must not hold Node open either.
    setTimeout: (fn, ms) => {
      const timer = setTimeout(fn, ms);
      if (timer && typeof timer.unref === 'function') timer.unref();
      return timer;
    },
    clearTimeout,
    CustomEvent: StubCustomEvent,
    Date: { now: () => clock },
    Map,
    JSON,
    String,
    Array
  };

  new Function(...Object.keys(sandbox), source)(...Object.values(sandbox));

  return {
    body,
    /** What the toast container holds, as `{kind, text}` per toast. */
    toasts: () =>
      (container ? container.children : []).map((toast) => ({
        kind: String(toast.className).replace('toast toast-', ''),
        text: toast.children.length ? String(toast.children[0].textContent) : ''
      })),
    /**
     * Fire one `htmx:responseError`, as htmx does for a 4xx/5xx answer.
     *
     * `from` models the half of htmx's `responseInfo` the listener reads:
     * `{ user: true }` for a click or a submit (the browser marks such an
     * event `isTrusted`), the default for a request the page issued itself
     * (`hx-trigger="load"` and polling call the issuer with no event at all,
     * so `triggeringEvent` is `undefined`), and `label` for a control carrying
     * `data-error-label`. `noConfig` drops `requestConfig` entirely, which is
     * the "htmx did not say" case.
     */
    respondWithError(xhr, from = {}) {
      body.dispatchEvent(
        new StubCustomEvent('htmx:responseError', { detail: responseDetail(xhr, from) })
      );
    },
    /**
     * Fire one of the events htmx raises when the request never got a
     * response: `xhr.onerror` → `htmx:sendError`, `xhr.ontimeout` →
     * `htmx:timeout`, `xhr.onabort` → `htmx:sendAbort`. Each carries the
     * request's `responseInfo`, which at that point has no status, no body and
     * no `successful` — that field is assigned only in `handleAjaxResponse`.
     */
    fireTransportEvent(type, from = {}) {
      body.dispatchEvent(new StubCustomEvent(type, { detail: responseDetail({}, from) }));
    },
    /**
     * An element with `attributes`, placed in the document under `id` when one
     * is given so `getElementById` finds it.
     */
    element(attributes = {}, id = null) {
      const el = new StubElement(attributes);
      if (id !== null) byId.set(id, el);
      return el;
    },
    /**
     * Fire one `htmx:afterRequest`, as htmx does when a request ends. htmx
     * sets `detail.successful` only for a 2xx it swapped; `elt` is the element
     * that issued the request.
     */
    finishRequest(elt, successful) {
      body.dispatchEvent(new StubCustomEvent('htmx:afterRequest', { detail: { elt, successful } }));
    },
    /** How many times the page asked to reload. */
    reloads: () => reloads,
    /** Move the clock the error listeners read, in milliseconds. */
    advance(ms) {
      clock += ms;
    }
  };
}

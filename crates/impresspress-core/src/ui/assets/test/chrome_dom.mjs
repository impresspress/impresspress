// A small document for the `chrome.js` modal tests (`chrome_modal.test.mjs`).
//
// `chrome_harness.mjs` stubs only what the toast and after-success sections
// read, and its `document` drops every listener. The modal section is the
// opposite case: it is ALL delegated listeners on `document` and `body`, it
// walks the tree (`closest`, `querySelectorAll`, `contains`), it tracks focus,
// and it drives `<dialog>`. So this is a tree, with event propagation (capture
// → target → bubble), focus, and a `<dialog>` whose `showModal()` / `close()`
// behave the way the HTML standard says the browser's do in the parts the
// section relies on:
//
// - `showModal()` focuses the first `[autofocus]` descendant, else the dialog;
// - `close()` returns focus to what was focused before `showModal()` — when
//   that element is still in the document — and fires `close` (which does not
//   bubble) from a QUEUED task, not synchronously. That ordering is load-bearing:
//   an htmx `closeModal` trigger closes the dialog, then htmx swaps, and only
//   then does `close` arrive;
// - removing an open dialog from the document closes it with no `close` event.
//
// No real DOM library is used for the reason `chrome_harness.mjs` gives for its
// stub: these tests run under a bare `node --test` in CI with nothing
// installed, and jsdom implements no `showModal()` anyway.
//
// Selectors: `matches`/`closest`/`querySelectorAll` understand exactly the
// grammar chrome.js uses — comma lists of compound selectors made of a tag
// name, `[attr]`, `[attr="value"]` and `:not(<those>)`.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const source = fs.readFileSync(path.join(here, '..', 'chrome.js'), 'utf8');

class StubEvent {
  constructor(type, init = {}) {
    this.type = type;
    this.bubbles = init.bubbles !== undefined ? init.bubbles : true;
    this.detail = init.detail !== undefined ? init.detail : null;
    this.key = init.key;
    this.shiftKey = !!init.shiftKey;
    this.metaKey = !!init.metaKey;
    this.ctrlKey = !!init.ctrlKey;
    this.altKey = !!init.altKey;
    this.clientX = init.clientX || 0;
    this.clientY = init.clientY || 0;
    this.defaultPrevented = false;
    this.propagationStopped = false;
    this.target = null;
  }
  preventDefault() {
    this.defaultPrevented = true;
  }
  stopPropagation() {
    this.propagationStopped = true;
  }
}

/** `new CustomEvent(type, {detail})`: bubbles only when asked, like the real one. */
class StubCustomEvent extends StubEvent {
  constructor(type, init = {}) {
    super(type, { bubbles: !!init.bubbles, detail: init.detail });
  }
}

// --- selectors -------------------------------------------------------------

function parseCompound(text) {
  const parts = [];
  let rest = text.trim();
  const tag = /^[a-z][a-z0-9]*/i.exec(rest);
  if (tag) {
    parts.push({ kind: 'tag', name: tag[0].toUpperCase() });
    rest = rest.slice(tag[0].length);
  }
  while (rest.length) {
    let m;
    if ((m = /^\[([a-z-]+)(?:="([^"]*)")?\]/i.exec(rest))) {
      parts.push({ kind: 'attr', name: m[1], value: m[2] });
    } else if ((m = /^:not\(([^)]*)\)/.exec(rest))) {
      parts.push({ kind: 'not', inner: parseCompound(m[1]) });
    } else if ((m = /^\.([a-z0-9_-]+)/i.exec(rest))) {
      parts.push({ kind: 'class', name: m[1] });
    } else {
      throw new Error(`stub selector engine cannot parse: ${text}`);
    }
    rest = rest.slice(m[0].length);
  }
  return parts;
}

function matchesCompound(el, parts) {
  return parts.every((p) => {
    if (p.kind === 'tag') return el.tagName === p.name;
    if (p.kind === 'class') return (el.getAttribute('class') || '').split(/\s+/).includes(p.name);
    if (p.kind === 'attr') {
      if (!el.hasAttribute(p.name)) return false;
      return p.value === undefined || el.getAttribute(p.name) === p.value;
    }
    return !matchesCompound(el, p.inner);
  });
}

function matchesSelector(el, selector) {
  return selector.split(',').some((one) => matchesCompound(el, parseCompound(one)));
}

// --- the tree --------------------------------------------------------------

class StubNode {
  constructor(doc) {
    this.ownerDocument = doc;
    this.parentNode = null;
    this.children = [];
    this._listeners = [];
  }

  addEventListener(type, listener, options) {
    const capture = options === true || !!(options && options.capture);
    this._listeners.push({ type, listener, capture });
  }

  removeEventListener(type, listener, options) {
    const capture = options === true || !!(options && options.capture);
    const at = this._listeners.findIndex(
      (l) => l.type === type && l.listener === listener && l.capture === capture
    );
    if (at >= 0) this._listeners.splice(at, 1);
  }

  _fire(event, phase) {
    for (const l of this._listeners.slice()) {
      if (l.type !== event.type) continue;
      if (phase === 'capture' && !l.capture) continue;
      if (phase === 'bubble' && l.capture) continue;
      l.listener.call(this, event);
    }
  }

  dispatchEvent(event) {
    event.target = this;
    const path = [];
    for (let n = this.parentNode; n; n = n.parentNode) path.unshift(n);
    for (const n of path) {
      n._fire(event, 'capture');
      if (event.propagationStopped) return !event.defaultPrevented;
    }
    this._fire(event, 'target');
    if (event.bubbles) {
      for (const n of path.slice().reverse()) {
        if (event.propagationStopped) break;
        n._fire(event, 'bubble');
      }
    }
    return !event.defaultPrevented;
  }

  appendChild(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
    return child;
  }

  removeChild(child) {
    const at = this.children.indexOf(child);
    if (at >= 0) this.children.splice(at, 1);
    child.parentNode = null;
    child._removed();
    return child;
  }

  _removed() {
    for (const c of this.children) c._removed();
  }

  get isConnected() {
    let n = this;
    while (n.parentNode) n = n.parentNode;
    return n === this.ownerDocument;
  }

  *descendants() {
    for (const c of this.children) {
      yield c;
      yield* c.descendants();
    }
  }

  querySelectorAll(selector) {
    return [...this.descendants()].filter((el) => matchesSelector(el, selector));
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] || null;
  }

  contains(other) {
    for (let n = other; n; n = n.parentNode) if (n === this) return true;
    return false;
  }
}

class StubElement extends StubNode {
  constructor(doc, tag, attributes = {}) {
    super(doc);
    this.tagName = tag.toUpperCase();
    this.attributes = {};
    for (const [k, v] of Object.entries(attributes)) this.setAttribute(k, v);
    this.textContent = '';
    this.className = attributes.class || '';
    this.value = attributes.value || '';
    this.type = attributes.type || '';
    // Rendered unless a test says otherwise: `getClientRects()` is what the
    // section reads to skip controls that are not displayed.
    this.displayed = true;
    // `getBoundingClientRect()`; a test sets it where geometry matters.
    this.box = { left: 0, top: 0, right: 100, bottom: 100 };
  }

  get id() {
    return this.getAttribute('id') || '';
  }

  setAttribute(name, value) {
    this.attributes[name] = String(value);
  }
  getAttribute(name) {
    return name in this.attributes ? this.attributes[name] : null;
  }
  hasAttribute(name) {
    return name in this.attributes;
  }
  removeAttribute(name) {
    delete this.attributes[name];
  }

  matches(selector) {
    return matchesSelector(this, selector);
  }

  closest(selector) {
    for (let n = this; n instanceof StubElement; n = n.parentNode) {
      if (n.matches(selector)) return n;
    }
    return null;
  }

  focus() {
    if (!this.isConnected || !this.displayed) return;
    this.ownerDocument.activeElement = this;
  }

  getClientRects() {
    return this.displayed ? [this.box] : [];
  }

  getBoundingClientRect() {
    return this.box;
  }

  remove() {
    if (this.parentNode) this.parentNode.removeChild(this);
  }

  _removed() {
    super._removed();
    if (this.ownerDocument.activeElement === this) this.ownerDocument.activeElement = null;
  }
}

/** A `<dialog>`, as far as the modal section uses one. */
class StubDialog extends StubElement {
  constructor(doc, attributes) {
    super(doc, 'dialog', attributes);
    this.open = false;
    this.modal = false;
    this.previouslyFocused = null;
    this.showModalCalls = 0;
  }

  showModal() {
    if (this.open) throw new Error('InvalidStateError: dialog already open');
    this.showModalCalls += 1;
    this.open = true;
    this.modal = true;
    this.previouslyFocused = this.ownerDocument.activeElement;
    const auto = this.querySelector('[autofocus]');
    (auto || this).displayed = true;
    if (auto) auto.focus();
    else this.ownerDocument.activeElement = this;
  }

  close() {
    if (!this.open) return;
    this.open = false;
    this.modal = false;
    const back = this.previouslyFocused;
    this.previouslyFocused = null;
    if (back && back.isConnected) back.focus();
    // Queued, as the standard has it.
    setTimeout(() => this.dispatchEvent(new StubEvent('close', { bubbles: false })), 0);
  }

  _removed() {
    // Removed while open: out of the top layer, no longer modal, no event.
    this.open = false;
    this.modal = false;
    super._removed();
  }
}

class StubDocument extends StubNode {
  constructor() {
    super(null);
    this.ownerDocument = this;
    this.documentElement = new StubElement(this, 'html');
    this.appendChild(this.documentElement);
    this.body = new StubElement(this, 'body');
    this.documentElement.appendChild(this.body);
    this.activeElement = this.body;
  }

  getElementById(id) {
    for (const el of this.descendants()) if (el.id === id) return el;
    return null;
  }

  createElement(tag) {
    return tag.toLowerCase() === 'dialog' ? new StubDialog(this, {}) : new StubElement(this, tag);
  }
}

/** Let queued tasks (the dialog's `close` event) run. */
export function tick() {
  return new Promise((resolve) => setTimeout(resolve, 5));
}

/**
 * Load `chrome.js` against a fresh document. Returns the document, element
 * builders, and the htmx/user events a test fires.
 */
export function loadChromeDom() {
  const doc = new StubDocument();
  const toastContainer = new StubElement(doc, 'div', { id: 'toast-container', popover: 'manual' });
  toastContainer.popoverOpen = false;
  toastContainer.popoverShows = 0;
  toastContainer.showPopover = function () {
    this.popoverOpen = true;
    this.popoverShows += 1;
  };
  toastContainer.hidePopover = function () {
    this.popoverOpen = false;
  };
  const baseMatches = toastContainer.matches.bind(toastContainer);
  toastContainer.matches = (selector) =>
    selector === ':popover-open' ? toastContainer.popoverOpen : baseMatches(selector);
  doc.body.appendChild(toastContainer);

  const window = {
    location: { reload() {} },
  };
  const sandbox = {
    window,
    document: doc,
    navigator: { platform: 'Linux' },
    Element: StubElement,
    CustomEvent: StubCustomEvent,
    setTimeout: (fn, ms) => {
      const timer = setTimeout(fn, ms);
      if (timer && typeof timer.unref === 'function') timer.unref();
      return timer;
    },
    clearTimeout,
    requestAnimationFrame: (fn) => setTimeout(fn, 0),
    Date,
    Map,
    JSON,
    String,
    Array,
    Math,
  };
  new Function(...Object.keys(sandbox), source)(...Object.values(sandbox));

  function el(tag, attributes = {}, children = []) {
    const node = tag === 'dialog' ? new StubDialog(doc, attributes) : new StubElement(doc, tag, attributes);
    for (const c of children) node.appendChild(c);
    return node;
  }

  return {
    doc,
    body: doc.body,
    toastContainer,
    el,
    /**
     * What `components::modal` renders, in the shape the section reads: a
     * dialog with a close button, and whatever `content` the test passes in
     * its body.
     */
    modal(id, content = []) {
      return el('dialog', { class: 'modal', id, 'aria-labelledby': `${id}-title` }, [
        el('div', { class: 'modal__header' }, [
          el('h2', { id: `${id}-title` }),
          el('button', { class: 'modal__close', type: 'button', 'data-action': 'modal-close', 'aria-label': 'Close' }),
        ]),
        el('div', { class: 'modal__body' }, content),
      ]);
    },
    /** A DOM event to dispatch by hand; `bubbles` defaults to true. */
    event(type, init = {}) {
      return new StubEvent(type, init);
    },
    /** A press and release on `target`, as a user's click is. */
    click(target, at = {}) {
      target.dispatchEvent(new StubEvent('mousedown', at));
      const e = new StubEvent('click', at);
      target.dispatchEvent(e);
      return e;
    },
    /** A key press on whatever has focus. */
    key(key, init = {}) {
      const target = doc.activeElement || doc.body;
      const e = new StubEvent('keydown', { key, ...init });
      target.dispatchEvent(e);
      return e;
    },
    /** htmx's request lifecycle, as fired on the issuing element and bubbled. */
    htmx: {
      beforeRequest(elt) {
        elt.dispatchEvent(new StubCustomEvent('htmx:beforeRequest', { bubbles: true, detail: { elt } }));
      },
      afterSwap(target) {
        target.dispatchEvent(new StubCustomEvent('htmx:afterSwap', { bubbles: true, detail: { target } }));
      },
      afterRequest(elt, successful = true) {
        // htmx fires it on the issuing element; once that element was swapped
        // out, the event no longer reaches `body` from it, so it goes to body.
        const from = elt.isConnected ? elt : doc.body;
        from.dispatchEvent(
          new StubCustomEvent('htmx:afterRequest', { bubbles: true, detail: { elt, successful } })
        );
      },
      /** An `HX-Trigger` header event, which htmx fires on the issuing element. */
      trigger(elt, name, detail) {
        const from = elt && elt.isConnected ? elt : doc.body;
        from.dispatchEvent(new StubCustomEvent(name, { bubbles: true, detail }));
      },
    },
  };
}

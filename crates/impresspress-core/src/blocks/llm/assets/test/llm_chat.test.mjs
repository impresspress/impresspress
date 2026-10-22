// `llm-chat.js` under `node --test`: the message cards it builds, how a
// thread link behaves on a phone, and what a sent chat shows when the server
// answers it or refuses it.
//
// No DOM library, for the reason `ui/assets/test/chrome_harness.mjs` gives:
// CI runs these with a bare `node --test` and nothing installed. The stub
// document below implements exactly what the script touches. Its elements
// have no `style` property at all, so a script that went back to painting
// inline styles (`el.style.x = …`) throws here instead of passing.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const source = fs.readFileSync(path.join(here, '..', 'llm-chat.js'), 'utf8');

const kebab = (camel) => 'data-' + camel.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase());

class El {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.attrs = new Map();
    this.children = [];
    this.parent = null;
    this._text = '';
    this._html = null;
    const self = this;
    this.dataset = new Proxy({}, {
      get: (_, k) => self.attrs.get(kebab(String(k))),
      set: (_, k, v) => { self.attrs.set(kebab(String(k)), String(v)); return true; },
    });
    this.classList = {
      contains: (c) => self.className.split(/\s+/).includes(c),
      remove: (c) => { self.className = self.className.split(/\s+/).filter((x) => x && x !== c).join(' '); },
      add: (c) => { if (!self.classList.contains(c)) self.className = (self.className + ' ' + c).trim(); },
      toggle: (c, on) => (on ? self.classList.add(c) : self.classList.remove(c)),
    };
  }
  get className() { return this.attrs.get('class') || ''; }
  set className(v) { this.attrs.set('class', String(v)); }
  get id() { return this.attrs.get('id') || ''; }
  set id(v) { this.attrs.set('id', String(v)); }
  setAttribute(k, v) { this.attrs.set(k, String(v)); }
  getAttribute(k) { return this.attrs.has(k) ? this.attrs.get(k) : null; }
  hasAttribute(k) { return this.attrs.has(k); }
  removeAttribute(k) { this.attrs.delete(k); }
  get textContent() { return this._html !== null ? this._html : this._text + this.children.map((c) => c.textContent).join(''); }
  set textContent(v) { this.children.forEach((c) => { c.parent = null; }); this.children = []; this._html = null; this._text = String(v); }
  get innerHTML() { return this._html; }
  set innerHTML(v) { this.children = []; this._text = ''; this._html = String(v); }
  appendChild(c) { if (c.parent) c.remove(); c.parent = this; this.children.push(c); return c; }
  insertBefore(c, ref) { c.parent = this; const i = ref ? this.children.indexOf(ref) : -1; if (i < 0) this.children.push(c); else this.children.splice(i, 0, c); return c; }
  get firstChild() { return this.children[0] || null; }
  get firstElementChild() { return this.children[0] || null; }
  cloneNode(deep) {
    const copy = new El(this.tagName);
    for (const [k, v] of this.attrs) copy.attrs.set(k, v);
    copy._text = this._text;
    copy._html = this._html;
    if (deep) for (const c of this.children) copy.appendChild(c.cloneNode(true));
    return copy;
  }
  remove() { if (this.parent) { this.parent.children = this.parent.children.filter((c) => c !== this); this.parent = null; } }
  matches(sel) { return matches(this, sel); }
  closest(sel) { for (let e = this; e; e = e.parent) if (e.matches && matches(e, sel)) return e; return null; }
  querySelectorAll(sel) { const out = []; const walk = (e) => { for (const c of e.children) { if (matches(c, sel)) out.push(c); walk(c); } }; walk(this); return out; }
  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; }
  focus() {}
}

function simple(el, sel) {
  if (sel.startsWith('.')) return sel.slice(1).split('.').every((c) => el.className.split(/\s+/).includes(c));
  if (sel.startsWith('[')) return el.attrs.has(sel.slice(1, -1));
  if (sel.startsWith('#')) return el.id === sel.slice(1);
  return el.tagName === sel.toUpperCase();
}
function matches(el, sel) {
  const parts = sel.split(/\s*>\s*/);
  if (!simple(el, parts.pop())) return false;
  let e = el.parent;
  while (parts.length) { if (!e || !simple(e, parts.pop())) return false; e = e.parent; }
  return true;
}

/** A chat page: `.page--chat` > (aside.chat-threads > #thread-list) + (section.chat-main > #messages-area + form#chat-form). */
function harness({ bootstrap = [], conversationDisplay = 'flex', reply = null, noMessagesTemplate = true } = {}) {
  const listeners = { click: [], submit: [], change: [], DOMContentLoaded: [] };
  const body = new El('body');
  const page = body.appendChild(new El('div'));
  page.className = 'page--chat';
  page.setAttribute('data-chat-focus', 'threads');
  const threads = page.appendChild(new El('aside'));
  threads.className = 'chat-threads';
  const list = threads.appendChild(new El('div'));
  list.id = 'thread-list';
  const main = page.appendChild(new El('section'));
  main.className = 'chat-main';
  const area = main.appendChild(new El('div'));
  area.id = 'messages-area';
  const form = main.appendChild(new El('form'));
  form.id = 'chat-form';
  form.className = 'chat-form chat-form--disabled';
  const hidden = form.appendChild(new El('input'));
  hidden.id = 'active-thread-id';
  hidden.value = '';
  const input = form.appendChild(new El('textarea'));
  input.id = 'chat-input';
  input.value = '';
  // The server-rendered note llm-chat.js clones for a thread with no
  // messages (`render_messages_pane`), outside `#messages-area`.
  if (noMessagesTemplate) {
    const template = main.appendChild(new El('template'));
    template.id = 'no-messages-note';
    template.content = new El('fragment');
    const note = template.content.appendChild(new El('p'));
    note.className = 'empty-note';
    note.textContent = 'No messages yet.';
  }
  const carrier = body.appendChild(new El('script'));
  carrier.id = 'llm-chat-bootstrap';
  carrier.textContent = JSON.stringify(bootstrap);

  const byId = (id) => body.querySelectorAll('#' + id)[0] || null;
  const fetches = [];
  const document = {
    body,
    getElementById: byId,
    createElement: (t) => new El(t),
    querySelector: (s) => body.querySelector(s),
    querySelectorAll: (s) => body.querySelectorAll(s),
    addEventListener: (type, fn) => { (listeners[type] = listeners[type] || []).push(fn); },
  };
  const window = {
    location: { assigned: null, assign(u) { this.assigned = u; } },
    getComputedStyle: (el) => ({ display: el === main ? conversationDisplay : 'block' }),
  };
  const context = {
    window, document,
    history: { replaced: null, replaceState(_s, _t, u) { this.replaced = u; } },
    marked: { parse: (t) => '<p>' + t + '</p>' },
    DOMPurify: { sanitize: (h) => h },
    fetch: (url, init) => {
      fetches.push(url);
      const answer = reply?.(url, init) ?? { status: 200, body: { records: [] } };
      return Promise.resolve({
        ok: answer.status >= 200 && answer.status < 300,
        status: answer.status,
        json: () => Promise.resolve(answer.body),
      });
    },
    setTimeout: () => 0,
    console,
    Element: El,
    Promise, JSON, Date, Math, String, Array,
  };
  context.window.window = context.window;
  vm.createContext(context);
  vm.runInContext(source, context);
  context.window.impresspressLlmChat.init();
  const click = (target) => {
    const e = { target, button: 0, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
    for (const fn of listeners.click) fn(e);
    return e;
  };
  const submit = () => {
    const e = { target: form, preventDefault() {} };
    for (const fn of listeners.submit) fn(e);
  };
  return { page, list, area, form, hidden, input, window, history: context.history, click, submit, fetches };
}

function threadLink(list, id) {
  const a = list.appendChild(new El('a'));
  a.className = 'thread-card';
  a.dataset.threadId = id;
  a.dataset.active = 'false';
  return a;
}

test('message cards use the shared card and badge classes, with no inline style', () => {
  const { area } = harness({
    bootstrap: [
      { role: 'user', content: '<b>not markup</b>', created_at: '2026-10-06T10:00:00Z' },
      { role: 'assistant', content: 'hello', created_at: '2026-10-06T10:00:01Z' },
      { role: 'system', content: 'Error: x', created_at: '' },
    ],
  });
  const [user, assistant, system] = area.children;
  assert.equal(user.className, 'message-card message-card--user');
  assert.equal(assistant.className, 'message-card message-card--neutral');
  assert.equal(system.className, 'message-card message-card--warning');

  // The role is written in sentence case at the source, as the server's
  // `entry_card` writes it: no class transforms its case.
  const userBadge = user.querySelector('.badge');
  assert.equal(userBadge.className, 'badge');
  assert.equal(userBadge.textContent, 'User');
  assert.equal(assistant.querySelector('.badge').textContent, 'Assistant');
  assert.equal(system.querySelector('.badge').className, 'badge badge-warning');
  assert.equal(user.querySelector('.message-card__date').textContent, '2026-10-06');

  // A user turn is text, never markup; an assistant turn is sanitized markdown.
  const userBody = user.querySelector('.message-card__content');
  assert.equal(userBody.innerHTML, null);
  assert.equal(userBody.textContent, '<b>not markup</b>');
  const assistantBody = assistant.querySelector('.message-card__content');
  assert.equal(assistantBody.className, 'message-card__content message-card__content--markdown');
  assert.equal(assistantBody.innerHTML, '<p>hello</p>');

  for (const card of area.children) {
    const all = [card, ...card.querySelectorAll('div'), ...card.querySelectorAll('span')];
    for (const el of all) assert.equal(el.getAttribute('style'), null, `${el.className} carries a style`);
  }
});

test('beside the list, a thread link opens the thread in place and marks it current', async () => {
  const h = harness({ conversationDisplay: 'flex' });
  const a = threadLink(h.list, 't-1');
  const other = threadLink(h.list, 't-2');
  other.setAttribute('aria-current', 'page');

  const e = h.click(a);
  assert.equal(e.defaultPrevented, true);
  assert.equal(h.hidden.value, 't-1');
  assert.equal(h.page.getAttribute('data-chat-focus'), 'conversation');
  assert.equal(h.form.className, 'chat-form', 'the composer is enabled by class, not by style');
  assert.equal(a.getAttribute('aria-current'), 'page');
  assert.equal(other.getAttribute('aria-current'), null);
  assert.equal(h.history.replaced, '/b/llm/threads/t-1');

  await new Promise((r) => setImmediate(r));
  assert.equal(h.area.children.length, 1);
  assert.equal(h.area.children[0].className, 'empty-note');
  assert.equal(h.area.children[0].textContent, 'No messages yet.');
});

test('a thread with no messages clones the server note and writes none itself', async () => {
  const h = harness({ conversationDisplay: 'flex', noMessagesTemplate: false });
  h.click(threadLink(h.list, 't-1'));
  await new Promise((r) => setImmediate(r));
  assert.equal(h.area.children.length, 0, 'no template, so no note: the script builds none');
});

test('the first message sent into an empty thread replaces its empty note', async () => {
  const h = harness({
    reply: (url) => (url === '/b/llm/api/chat'
      ? { status: 200, body: { content: 'hello', message_id: 'e-1', model: 'm', truncated: false } }
      : null),
  });
  h.click(threadLink(h.list, 't-1'));
  await new Promise((r) => setImmediate(r));
  assert.equal(h.area.children[0].className, 'empty-note');

  h.input.value = 'hi';
  h.submit();
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(
    h.area.children.map((c) => c.className),
    ['message-card message-card--user', 'message-card message-card--neutral'],
  );
});

test('on a phone showing the list, a thread link navigates instead', () => {
  const h = harness({ conversationDisplay: 'none' });
  const a = threadLink(h.list, 't-1');
  const e = h.click(a);
  assert.equal(e.defaultPrevented, false, 'the link must take the browser to the thread page');
  assert.equal(h.hidden.value, '');
  assert.equal(h.history.replaced, null);
});

/** Let every pending promise in the page settle. */
const settled = () => new Promise((r) => setImmediate(r));

test('a chat the server refuses shows the reason it gives, not "No response"', async () => {
  const reason = "llm.chat: no open page has the LLM model 'm' loaded — load it first";
  const h = harness({
    reply: (url) => (url === '/b/llm/api/chat'
      ? { status: 503, body: { error: 'Unavailable', message: reason } }
      : null),
  });
  h.hidden.value = 't-1';
  h.input.value = 'hi';
  h.submit();
  await settled();

  assert.ok(h.fetches.includes('/b/llm/api/chat'), h.fetches.join(', '));
  const cards = h.area.children;
  const last = cards[cards.length - 1];
  assert.equal(last.className, 'message-card message-card--warning', 'the refusal is a system card');
  assert.equal(last.querySelector('.message-card__content').textContent, 'Error: ' + reason);
  assert.ok(
    !cards.some((c) => (c.querySelector('.message-card__content')?.textContent ?? '').includes('No response')),
    'no assistant card claims an empty answer',
  );
});

test('a chat the server answers is shown as the reply', async () => {
  const h = harness({
    reply: (url) => (url === '/b/llm/api/chat'
      ? { status: 200, body: { content: 'hello', message_id: 'e-1', model: 'm', truncated: false } }
      : null),
  });
  h.hidden.value = 't-1';
  h.input.value = 'hi';
  h.submit();
  await settled();

  const cards = h.area.children;
  const last = cards[cards.length - 1];
  assert.equal(last.className, 'message-card message-card--neutral');
  assert.equal(last.querySelector('.message-card__content').innerHTML, '<p>hello</p>');
});

test('New thread adds a thread link, not a card, and drops the empty note', async () => {
  const h = harness({
    reply: (url, init) => (url === '/b/messages/api/contexts' && init?.method === 'POST'
      ? { status: 201, body: { id: 't-9' } }
      : null),
  });
  // The server's "No threads yet" line (`components::empty_note`).
  const empty = h.list.appendChild(new El('p'));
  empty.className = 'empty-note';
  const button = h.page.appendChild(new El('button'));
  button.setAttribute('data-action', 'llm-new-thread');

  h.click(button);
  await settled();

  assert.equal(h.list.children.length, 1, 'the note is gone and one link is listed');
  const [link] = h.list.children;
  assert.equal(link.tagName, 'A');
  // A thread is a link in the list (`thread_list_items` in pages.rs), never a
  // `components::Card`: `card` as a class of its own is the page-level surface.
  assert.equal(link.className, 'thread-card');
  assert.ok(!link.className.split(/\s+/).includes('card'), link.className);
  assert.equal(link.dataset.threadId, 't-9');
  assert.equal(link.querySelector('.thread-card__title').textContent, 'New chat');
  assert.equal(link.querySelector('.thread-card__date').className, 'text-muted thread-card__date');
});

// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_confirm.test.mjs
//
// Pins `chrome.js` section 4's confirm half: the one page-built dialog every
// `hx-confirm` and every `window.impresspressConfirm` asks in, what it shows,
// how it answers, and where focus goes once a confirmed request has swapped
// out the control that asked. The browser half — the real `showModal()`,
// Tab, Esc and htmx — is `crates/impresspress-web/tests/e2e/confirm-dialog.spec.ts`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChromeDom, tick } from './chrome_dom.mjs';

/** What `components::confirm_dialog` renders, in the shape chrome.js reads. */
function confirmDialog(page) {
  const { el } = page;
  const cancel = el('button', { class: 'btn btn--secondary', type: 'button', 'data-action': 'modal-close', autofocus: '' });
  const danger = el('button', { 'data-confirm-tone': 'danger', class: 'btn btn--danger', hidden: '', type: 'button', 'data-action': 'confirm-accept' });
  const primary = el('button', { 'data-confirm-tone': 'primary', class: 'btn btn--primary', hidden: '', type: 'button', 'data-action': 'confirm-accept' });
  const title = el('h2', { class: 'modal__title', id: 'confirm-dialog-title' });
  const question = el('p', { class: 'confirm-dialog__question', id: 'confirm-dialog-question' });
  const dialog = el(
    'dialog',
    { class: 'modal', id: 'confirm-dialog', role: 'alertdialog', 'aria-labelledby': 'confirm-dialog-title', 'aria-describedby': 'confirm-dialog-question' },
    [
      el('div', { class: 'modal__header' }, [
        el('div', { class: 'modal__heading' }, [title]),
        el('button', { class: 'modal__close', type: 'button', 'data-action': 'modal-close', 'aria-label': 'Close' }),
      ]),
      el('div', { class: 'modal__body' }, [question, el('div', { class: 'modal__footer' }, [cancel, danger, primary])]),
    ]
  );
  page.body.appendChild(dialog);
  return { dialog, title, question, cancel, danger, primary };
}

/** `#content` holding a table of `n` rows, each with an Edit and a Revoke button. */
function pageWithRows(page, n, { tableId = null } = {}) {
  const { el } = page;
  const content = el('main', { id: 'content', tabindex: '0' });
  const tbody = el('tbody');
  const rows = [];
  for (let i = 0; i < n; i++) {
    const edit = el('button', { type: 'button', 'data-row': String(i) });
    const revoke = el('button', {
      type: 'button',
      'hx-delete': `/rows/${i}`,
      'hx-confirm': `Revoke row ${i}?`,
      'data-confirm-label': 'Revoke row',
      'data-confirm-tone': 'danger',
      'data-row': String(i),
    });
    const row = el('tr', {}, [el('td', {}, [edit]), el('td', {}, [revoke])]);
    tbody.appendChild(row);
    rows.push({ row, edit, revoke });
  }
  const table = el('table', tableId ? { id: tableId } : {}, [tbody]);
  content.appendChild(table);
  page.body.appendChild(content);
  return { content, table, tbody, rows };
}

/** Ask through htmx from `opener`, press the confirm button, let `close` land. */
async function askAndConfirm(page, opener, target, ui) {
  opener.focus();
  const asked = page.htmx.confirm(opener, opener.getAttribute('hx-confirm'), target);
  page.click(ui.danger.hasAttribute('hidden') ? ui.primary : ui.danger);
  await tick();
  return asked;
}

test('an hx-confirm asks in the shared dialog, in the action\'s words and tone, Cancel focused', () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows } = pageWithRows(page, 1);
  const opener = rows[0].revoke;
  opener.focus();

  const { event, issued } = page.htmx.confirm(opener, 'Revoke row 0?');

  assert.equal(event.defaultPrevented, true, 'htmx does not ask window.confirm');
  assert.equal(ui.dialog.open, true);
  assert.equal(ui.dialog.modal, true);
  assert.equal(ui.title.textContent, 'Revoke row');
  assert.equal(ui.question.textContent, 'Revoke row 0?');
  assert.equal(ui.danger.textContent, 'Revoke row');
  assert.equal(ui.danger.hasAttribute('hidden'), false, 'a danger action shows the danger button');
  assert.equal(ui.primary.hasAttribute('hidden'), true, 'and only that one');
  assert.equal(page.doc.activeElement, ui.cancel, 'Cancel has focus: a stray Enter cancels');
  assert.deepEqual(issued, [], 'nothing is sent before the answer');
});

test('Cancel, Esc and the close button answer no: no request, focus back on the opener', async () => {
  for (const how of ['cancel', 'escape', 'close']) {
    const page = loadChromeDom();
    const ui = confirmDialog(page);
    const { rows } = pageWithRows(page, 1);
    const opener = rows[0].revoke;
    opener.focus();
    const { issued } = page.htmx.confirm(opener, 'Revoke row 0?');

    if (how === 'cancel') page.click(ui.cancel);
    else if (how === 'escape') page.escape(ui.dialog);
    else page.click(ui.dialog.querySelector('.modal__close'));
    await tick();

    assert.equal(ui.dialog.open, false, how);
    assert.deepEqual(issued, [], how);
    assert.equal(page.doc.activeElement, opener, how);
  }
});

test('the confirm button answers yes: the request goes once, skipping htmx\'s own confirm, from the opener', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows } = pageWithRows(page, 1);
  const opener = rows[0].revoke;
  let focusedWhenSent = null;
  opener.focus();
  const asked = page.htmx.confirm(opener, 'Revoke row 0?');
  const issue = asked.event.detail.issueRequest;
  asked.event.detail.issueRequest = (skip) => {
    focusedWhenSent = page.doc.activeElement;
    issue(skip);
  };

  page.click(ui.danger);
  await tick();

  assert.equal(ui.dialog.open, false);
  assert.deepEqual(asked.issued, [true]);
  assert.equal(focusedWhenSent, opener, 'the action starts where the reader asked it');
});

test('a primary tone shows the primary button and hides the danger one', () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows } = pageWithRows(page, 1);
  const opener = rows[0].revoke;
  opener.setAttribute('data-confirm-label', 'Load model');
  opener.setAttribute('data-confirm-tone', 'primary');

  page.htmx.confirm(opener, 'Load it?');

  assert.equal(ui.primary.hasAttribute('hidden'), false);
  assert.equal(ui.primary.textContent, 'Load model');
  assert.equal(ui.danger.hasAttribute('hidden'), true);
});

test('the label and tone are read off the element that holds hx-confirm', () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const form = page.el('form', { 'hx-confirm': 'Send it?', 'data-confirm-label': 'Send', 'data-confirm-tone': 'primary' });
  const submit = page.el('button', { type: 'submit' });
  form.appendChild(submit);
  page.body.appendChild(form);

  page.htmx.confirm(submit, 'Send it?');

  assert.equal(ui.title.textContent, 'Send');
});

test('a request with no question is not asked about', () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const button = page.el('button', { 'hx-post': '/x' });
  page.body.appendChild(button);

  const { event } = page.htmx.confirm(button, null);

  assert.equal(event.defaultPrevented, false, 'htmx goes ahead on its own');
  assert.equal(ui.dialog.open, false);
});

test('an hx-confirm without a label or tone is refused loudly, and never sent', () => {
  for (const [label, tone] of [[null, 'danger'], ['', 'danger'], ['Revoke', null], ['Revoke', 'ok']]) {
    const page = loadChromeDom();
    const ui = confirmDialog(page);
    const button = page.el('button', { 'hx-delete': '/x', 'hx-confirm': 'Revoke?' });
    if (label !== null) button.setAttribute('data-confirm-label', label);
    if (tone !== null) button.setAttribute('data-confirm-tone', tone);
    page.body.appendChild(button);

    let issued = 0;
    const e = new (page.event('x').constructor)('htmx:confirm', {
      detail: { elt: button, question: 'Revoke?', issueRequest: () => (issued += 1) },
    });
    assert.throws(() => button.dispatchEvent(e), TypeError, `${label}/${tone}`);
    assert.equal(e.defaultPrevented, true, 'htmx does not fall back to window.confirm');
    assert.equal(ui.dialog.open, false);
    assert.equal(issued, 0);
  }
});

test('window.impresspressConfirm answers a script with a promise of the reader\'s answer', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const opener = page.el('button', { type: 'button' });
  page.body.appendChild(opener);
  opener.focus();

  const yes = page.window.impresspressConfirm({ question: 'Delete a.txt?', label: 'Delete file', tone: 'danger' });
  assert.equal(ui.dialog.open, true);
  assert.equal(ui.title.textContent, 'Delete file');
  page.click(ui.danger);
  assert.equal(await yes, true);
  assert.equal(page.doc.activeElement, opener);

  const no = page.window.impresspressConfirm({ question: 'Delete b.txt?', label: 'Delete file', tone: 'danger', opener });
  page.escape(ui.dialog);
  assert.equal(await no, false);
  assert.equal(page.doc.activeElement, opener);
});

test('window.impresspressConfirm refuses a question without its words or tone', () => {
  const page = loadChromeDom();
  confirmDialog(page);
  for (const asked of [
    undefined,
    { label: 'Delete', tone: 'danger' },
    { question: 'Delete?', tone: 'danger' },
    { question: 'Delete?', label: 'Delete' },
    { question: 'Delete?', label: 'Delete', tone: 'warning' },
  ]) {
    assert.throws(() => page.window.impresspressConfirm(asked), TypeError, JSON.stringify(asked));
  }
});

test('a page with no confirm dialog says so rather than confirming nothing', () => {
  const page = loadChromeDom();
  assert.throws(
    () => page.window.impresspressConfirm({ question: 'Delete?', label: 'Delete', tone: 'danger' }),
    /no #confirm-dialog/
  );
});

test('one question at a time: a second ask while one is open answers no', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const first = page.window.impresspressConfirm({ question: 'One?', label: 'One', tone: 'danger' });
  const second = page.window.impresspressConfirm({ question: 'Two?', label: 'Two', tone: 'danger' });
  assert.equal(await second, false);
  assert.equal(ui.title.textContent, 'One');
  page.click(ui.danger);
  assert.equal(await first, true);
});

// --- focus once a confirmed request has landed ------------------------------

test('a removed row hands focus to the same control in the row that moved up', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows } = pageWithRows(page, 3);
  const opener = rows[1].revoke;
  await askAndConfirm(page, opener, rows[1].row, ui);

  // `hx-target="closest tr" hx-swap="outerHTML"` with an empty answer.
  rows[1].row.remove();
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, rows[2].revoke);
});

test('the last row removed hands focus to the row before it', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows } = pageWithRows(page, 2);
  const opener = rows[1].revoke;
  await askAndConfirm(page, opener, rows[1].row, ui);

  rows[1].row.remove();
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, rows[0].revoke);
});

test('a row replaced in place hands focus to its replacement\'s control', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows, tbody } = pageWithRows(page, 2);
  const opener = rows[0].revoke;
  await askAndConfirm(page, opener, rows[0].row, ui);

  // Disable → Enable: the row comes back with a different control.
  const enable = page.el('button', { type: 'button' });
  const replacement = page.el('tr', {}, [page.el('td', {}, [page.el('button', { type: 'button' })]), page.el('td', {}, [enable])]);
  tbody.insertBefore(replacement, rows[0].row);
  rows[0].row.remove();
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, enable);
});

test('a list re-rendered whole is found again by its place in the request\'s target', async () => {
  for (const swap of ['innerHTML', 'outerHTML']) {
    const page = loadChromeDom();
    const ui = confirmDialog(page);
    const { rows, content, table } = pageWithRows(page, 3, { tableId: 'rows-table' });
    const opener = rows[0].revoke;
    const target = swap === 'innerHTML' ? content : table;
    await askAndConfirm(page, opener, target, ui);

    // The answer re-renders the list without row 0.
    const fresh = page.el('tbody');
    const kept = [1, 2].map((i) => {
      const revoke = page.el('button', { type: 'button', 'data-row': String(i) });
      fresh.appendChild(page.el('tr', {}, [page.el('td', {}, [page.el('button', { type: 'button' })]), page.el('td', {}, [revoke])]));
      return revoke;
    });
    const freshTable = page.el('table', { id: 'rows-table' }, [fresh]);
    if (swap === 'innerHTML') {
      table.remove();
      content.appendChild(freshTable);
    } else {
      content.insertBefore(freshTable, table);
      table.remove();
    }
    page.htmx.afterRequest(opener);

    assert.equal(page.doc.activeElement, kept[0], swap);
  }
});

test('with no row and no empty state left to hold it, focus goes to the page body', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows, content } = pageWithRows(page, 1);
  const opener = rows[0].revoke;
  await askAndConfirm(page, opener, rows[0].row, ui);

  rows[0].row.remove();
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, content);
});

test('the last row deleted and the list answered by its empty state: focus goes to the empty state\'s action', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows, content, table } = pageWithRows(page, 1);
  const opener = rows[0].revoke;
  await askAndConfirm(page, opener, content, ui);

  // The list re-rendered into `#content` as `components::empty_state`.
  const add = page.el('button', { type: 'button', class: 'btn btn--secondary' });
  table.remove();
  content.appendChild(page.el('div', { class: 'empty' }, [page.el('h2', { class: 'empty__title' }), page.el('div', { class: 'empty__action' }, [add])]));
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, add);
});

test('the empty state is the deleted row\'s own card\'s, not an earlier card\'s', async () => {
  const page = loadChromeDom();
  const { el } = page;
  const ui = confirmDialog(page);
  const content = el('main', { id: 'content', tabindex: '0' });
  const revoke = el('button', { type: 'button', 'hx-delete': '/rows/0', 'hx-confirm': 'Revoke row 0?', 'data-confirm-label': 'Revoke row', 'data-confirm-tone': 'danger' });
  content.appendChild(el('section', { class: 'card' }, [el('p', { class: 'empty-note' })]));
  content.appendChild(el('section', { class: 'card' }, [el('table', {}, [el('tbody', {}, [el('tr', {}, [el('td', {}, [revoke])])])])]));
  page.body.appendChild(content);
  await askAndConfirm(page, revoke, content, ui);

  // `hx-target="#content"`: both cards again, the second now empty.
  [...content.children].forEach((c) => c.remove());
  const add = el('button', { type: 'button' });
  content.appendChild(el('section', { class: 'card' }, [el('p', { class: 'empty-note' })]));
  content.appendChild(el('section', { class: 'card' }, [el('div', { class: 'empty' }, [el('div', { class: 'empty__action' }, [add])])]));
  page.htmx.afterRequest(revoke);

  assert.equal(page.doc.activeElement, add);
});

test('an empty state with no action takes focus itself, from script only', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows, content, table } = pageWithRows(page, 1);
  const opener = rows[0].revoke;
  await askAndConfirm(page, opener, content, ui);

  const note = page.el('p', { class: 'empty-note' });
  table.remove();
  content.appendChild(note);
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, note);
  assert.equal(note.getAttribute('tabindex'), '-1');
});

test('an empty state outside the request\'s target is not this list\'s', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows, content } = pageWithRows(page, 1);
  const elsewhere = page.el('button', { type: 'button' });
  content.appendChild(page.el('div', { class: 'empty' }, [elsewhere]));
  const opener = rows[0].revoke;
  await askAndConfirm(page, opener, rows[0].row, ui);

  rows[0].row.remove();
  page.htmx.afterRequest(opener);

  assert.equal(page.doc.activeElement, content);
});

// --- the script path: a block's own re-render -------------------------------

test('a script that re-renders its own list places focus as the htmx path does', async () => {
  const page = loadChromeDom();
  const { rows, content, table } = pageWithRows(page, 3);
  const opener = rows[1].revoke;
  opener.focus();
  const keep = page.window.impresspressKeepFocus(opener, content);

  // The script's own re-render: the whole table again, without row 1.
  const fresh = page.el('tbody');
  const kept = [0, 2].map((i) => {
    const revoke = page.el('button', { type: 'button', 'data-row': String(i) });
    fresh.appendChild(page.el('tr', {}, [page.el('td', {}, [page.el('button', { type: 'button' })]), page.el('td', {}, [revoke])]));
    return revoke;
  });
  table.remove();
  content.appendChild(page.el('table', {}, [fresh]));
  keep.restore();

  assert.equal(page.doc.activeElement, kept[1]);
});

test('a script\'s own rows, named by a selector, and its own empty list', () => {
  const page = loadChromeDom();
  const { el } = page;
  const content = el('main', { id: 'content', tabindex: '0' });
  const target = el('div', {});
  const row = (n) => {
    const archive = el('button', { type: 'button', 'data-n': String(n) });
    return { row: el('div', { class: 'preset-row' }, [el('strong'), archive]), archive };
  };
  const rows = [row(0), row(1)];
  rows.forEach((r) => target.appendChild(r.row));
  content.appendChild(target);
  page.body.appendChild(content);

  rows[0].archive.focus();
  let keep = page.window.impresspressKeepFocus(rows[0].archive, target, '.preset-row');
  rows[0].row.remove();
  keep.restore();
  assert.equal(page.doc.activeElement, rows[1].archive);

  keep = page.window.impresspressKeepFocus(rows[1].archive, target, '.preset-row');
  rows[1].row.remove();
  target.textContent = 'No saved presets yet.';
  keep.restore();
  assert.equal(page.doc.activeElement, target);
  assert.equal(target.getAttribute('tabindex'), '-1');
});

test('a script\'s restore leaves focus alone when the opener is still there, and needs the opener', () => {
  const page = loadChromeDom();
  const { rows, content } = pageWithRows(page, 2);
  const opener = rows[0].revoke;
  opener.focus();
  const keep = page.window.impresspressKeepFocus(opener, content);
  rows[1].edit.focus();
  keep.restore();
  assert.equal(page.doc.activeElement, rows[1].edit);
  assert.throws(() => page.window.impresspressKeepFocus(null, content), TypeError);
  assert.throws(() => page.window.impresspressKeepFocus(opener, null), TypeError);
});

test('a control still there, or focus the response placed, is left alone', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const { rows } = pageWithRows(page, 2);
  const opener = rows[0].revoke;
  await askAndConfirm(page, opener, rows[0].row, ui);
  // An `hx-swap="none"` action: nothing moved.
  page.htmx.afterRequest(opener);
  assert.equal(page.doc.activeElement, opener);

  const other = rows[1].revoke;
  await askAndConfirm(page, other, rows[1].row, ui);
  rows[1].row.remove();
  // The response focused a field of its own.
  const field = page.el('input', { id: 'named' });
  page.body.appendChild(field);
  field.focus();
  page.htmx.afterRequest(other);
  assert.equal(page.doc.activeElement, field);
});

test('asked again before the last answer\'s close event lands: the last answer stands, and the new question stays open', async () => {
  const page = loadChromeDom();
  const ui = confirmDialog(page);
  const opener = page.el('button', { type: 'button' });
  page.body.appendChild(opener);
  opener.focus();

  const first = page.window.impresspressConfirm({ question: 'One?', label: 'One', tone: 'danger' });
  page.escape(ui.dialog);
  // The `close` event is still queued: the reader asks again at once.
  const second = page.window.impresspressConfirm({ question: 'Two?', label: 'Two', tone: 'danger' });
  assert.equal(await first, false);
  assert.equal(ui.dialog.open, true);
  assert.equal(ui.title.textContent, 'Two');

  // The late `close` of the first answer undoes nothing of the second.
  await tick();
  assert.equal(ui.dialog.open, true);
  page.click(ui.danger);
  assert.equal(await second, true);
  assert.equal(page.doc.activeElement, opener);
});

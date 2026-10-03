// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_modal.test.mjs
//
// Pins `chrome.js` section 4's modal half: every way a `components::modal`
// <dialog> is opened or closed, and where focus goes each time. The browser
// half of the same contract — that the real `showModal()` makes the page inert
// and that Esc closes it — is `crates/impresspress-web/tests/e2e/admin-modals.spec.ts`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChromeDom, tick } from './chrome_dom.mjs';

/** A page with a trigger for modal `id` and the modal itself, both in `#content`. */
function pageWithModal(page, id = 'create-role', fields = null) {
  const content = page.el('div', { id: 'content' });
  const trigger = page.el('button', { 'data-action': 'modal-open', 'data-modal-target': id });
  const name = page.el('input', { id: 'role-name', name: 'name', autofocus: '' });
  const cancel = page.el('button', { type: 'button', 'data-action': 'modal-close' });
  const submit = page.el('button', { type: 'submit' });
  const form = page.el('form', {}, fields || [name, page.el('div', { class: 'modal__footer' }, [cancel, submit])]);
  const dialog = page.modal(id, [form]);
  content.appendChild(trigger);
  content.appendChild(dialog);
  page.body.appendChild(content);
  return { content, trigger, dialog, name, cancel, submit };
}

test('modal-open opens the dialog modally and moves focus into it', () => {
  const page = loadChromeDom();
  const { trigger, dialog, name } = pageWithModal(page);
  trigger.focus();

  const e = page.click(trigger);

  assert.equal(dialog.open, true);
  assert.equal(dialog.modal, true, 'opened with showModal(), not show()');
  assert.equal(page.doc.activeElement, name, 'the autofocus field has focus');
  assert.equal(e.defaultPrevented, true);
});

test('a modal-open naming no dialog does nothing, and does not throw', () => {
  const page = loadChromeDom();
  const stray = page.el('div', { id: 'not-a-dialog' });
  page.body.appendChild(stray);
  const trigger = page.el('button', { 'data-action': 'modal-open', 'data-modal-target': 'not-a-dialog' });
  const missing = page.el('button', { 'data-action': 'modal-open', 'data-modal-target': 'missing' });
  page.body.appendChild(trigger);
  page.body.appendChild(missing);

  assert.doesNotThrow(() => page.click(trigger));
  assert.doesNotThrow(() => page.click(missing));
});

test('opening an open modal again is a no-op, not an InvalidStateError', () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  page.click(trigger);

  assert.doesNotThrow(() =>
    page.htmx.trigger(null, 'openModal', { id: dialog.id })
  );
  assert.equal(dialog.showModalCalls, 1);
});

test('the close button closes its own modal and focus returns to the opener', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  trigger.focus();
  page.click(trigger);

  page.click(dialog.querySelector('.modal__close'));
  await tick();

  assert.equal(dialog.open, false);
  assert.equal(page.doc.activeElement, trigger);
});

test('Cancel closes the modal it sits in', async () => {
  const page = loadChromeDom();
  const { trigger, dialog, cancel } = pageWithModal(page);
  trigger.focus();
  page.click(trigger);

  page.click(cancel);
  await tick();

  assert.equal(dialog.open, false);
  assert.equal(page.doc.activeElement, trigger);
});

test('a modal closed by the browser (Esc) gives focus back to the opener', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  trigger.focus();
  page.click(trigger);
  // Focus moved on inside the dialog since it opened.
  dialog.querySelector('button[type="submit"]').focus();

  // What the UA does on Esc: `cancel`, then close the dialog.
  dialog.close();
  await tick();

  assert.equal(dialog.open, false);
  assert.equal(page.doc.activeElement, trigger);
});

test('Tab past the last control wraps to the first, and Shift+Tab the other way', () => {
  const page = loadChromeDom();
  const { trigger, dialog, submit } = pageWithModal(page);
  page.click(trigger);
  const close = dialog.querySelector('.modal__close');

  submit.focus();
  const forward = page.key('Tab');
  assert.equal(page.doc.activeElement, close, 'wrapped to the first control');
  assert.equal(forward.defaultPrevented, true);

  const backward = page.key('Tab', { shiftKey: true });
  assert.equal(page.doc.activeElement, submit, 'wrapped back to the last control');
  assert.equal(backward.defaultPrevented, true);
});

test('once the modal has closed, Tab is the page\'s again', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  trigger.focus();
  page.click(trigger);
  page.click(dialog.querySelector('.modal__close'));
  await tick();

  const e = page.key('Tab');

  assert.equal(e.defaultPrevented, false);
  assert.equal(page.doc.activeElement, trigger);
});

test('Tab in the middle of the dialog is left to the browser', () => {
  const page = loadChromeDom();
  const { trigger, name } = pageWithModal(page);
  page.click(trigger);

  name.focus();
  const e = page.key('Tab');

  assert.equal(e.defaultPrevented, false);
  assert.equal(page.doc.activeElement, name, 'the stub has no native Tab; nothing moved it');
});

test('Tab skips controls that are not displayed and disabled ones', () => {
  const page = loadChromeDom();
  const visible = page.el('input', { id: 'a', autofocus: '' });
  const hiddenGroup = page.el('input', { id: 'b' });
  hiddenGroup.displayed = false;
  const disabled = page.el('button', { disabled: '' });
  const { trigger, dialog } = pageWithModal(page, 'grant', [visible, hiddenGroup, disabled]);
  page.click(trigger);

  visible.focus();
  page.key('Tab');

  assert.equal(page.doc.activeElement, dialog.querySelector('.modal__close'));
});

/** A page with a trigger for a modal of reference content: no form in it. */
function pageWithReferenceModal(page, id = 'block-detail') {
  const trigger = page.el('button', { 'data-action': 'modal-open', 'data-modal-target': id });
  const link = page.el('a', { href: '/b/x/admin' });
  const dialog = page.modal(id, [page.el('p'), link]);
  page.body.appendChild(trigger);
  page.body.appendChild(dialog);
  return { trigger, dialog, link };
}

test('a click on the backdrop closes a reference modal; a click inside does not', async () => {
  const page = loadChromeDom();
  const { trigger, dialog, link } = pageWithReferenceModal(page);
  dialog.box = { left: 100, top: 100, right: 600, bottom: 400 };
  trigger.focus();
  page.click(trigger);

  page.click(link, { clientX: 200, clientY: 200 });
  assert.equal(dialog.open, true, 'a click on a link inside');
  // The dialog element's own padding is inside its box.
  page.click(dialog, { clientX: 150, clientY: 150 });
  assert.equal(dialog.open, true, 'a click on the dialog inside its box');

  page.click(dialog, { clientX: 20, clientY: 20 });
  await tick();
  assert.equal(dialog.open, false, 'a click outside the box, on the backdrop');
  assert.equal(page.doc.activeElement, trigger);
});

test('a modal with a form is never closed from the backdrop', () => {
  const page = loadChromeDom();
  const { trigger, dialog, name } = pageWithModal(page);
  dialog.box = { left: 100, top: 100, right: 600, bottom: 400 };
  page.click(trigger);
  name.value = 'typed';

  page.click(dialog, { clientX: 20, clientY: 20 });

  assert.equal(dialog.open, true);
  assert.equal(name.value, 'typed');
});

test('a drag that starts inside and ends on the backdrop keeps a reference modal open', () => {
  const page = loadChromeDom();
  const { trigger, dialog, link: name } = pageWithReferenceModal(page);
  dialog.box = { left: 100, top: 100, right: 600, bottom: 400 };
  page.click(trigger);

  // Press in the field, release over the backdrop: the browser fires the
  // click on the common ancestor, the dialog, at the release point.
  name.dispatchEvent(page.event('mousedown', { clientX: 200, clientY: 200 }));
  dialog.dispatchEvent(page.event('click', { clientX: 20, clientY: 20 }));

  assert.equal(dialog.open, true);
});

test('an htmx-loaded modal opens on the openModal trigger, and focus returns to the requester', async () => {
  const page = loadChromeDom();
  // A row's Edit button issues `hx-get`; the answer is the whole dialog,
  // swapped into a slot, with `HX-Trigger-After-Swap: openModal`.
  const edit = page.el('button', { 'hx-get': '/b/admin/variables/K/edit' });
  const slot = page.el('div', { id: 'edit-var-slot' });
  page.body.appendChild(edit);
  page.body.appendChild(slot);
  edit.focus();

  page.htmx.beforeRequest(edit);
  const dialog = page.modal('edit-var', [page.el('input', { id: 'edit-value', autofocus: '' })]);
  slot.appendChild(dialog);
  page.htmx.afterSwap(slot);
  page.htmx.trigger(edit, 'openModal', { id: 'edit-var' });
  page.htmx.afterRequest(edit);

  assert.equal(dialog.open, true);
  assert.equal(page.doc.activeElement.id, 'edit-value');

  dialog.close();
  await tick();
  assert.equal(page.doc.activeElement, edit);
});

test('a modal opened from a click that focused nothing returns focus to the requesting element', async () => {
  const page = loadChromeDom();
  // A block card is a `div` with `hx-get`: clicking it focuses nothing.
  const card = page.el('div', { 'hx-get': '/b/admin/blocks/x/detail', tabindex: '0' });
  const slot = page.el('div', { id: 'block-detail-slot' });
  page.body.appendChild(card);
  page.body.appendChild(slot);
  assert.equal(page.doc.activeElement, page.body);

  page.htmx.beforeRequest(card);
  const dialog = page.modal('block-detail');
  slot.appendChild(dialog);
  page.htmx.afterSwap(slot);
  page.htmx.trigger(card, 'openModal', { id: 'block-detail' });
  page.htmx.afterRequest(card);
  assert.equal(dialog.open, true);

  page.click(dialog.querySelector('.modal__close'));
  await tick();
  assert.equal(page.doc.activeElement, card);
});

test('closeModal from HX-Trigger closes the modal, and focus lands on the re-rendered trigger', async () => {
  const page = loadChromeDom();
  const { content, trigger, dialog, submit } = pageWithModal(page);
  trigger.focus();
  page.click(trigger);
  submit.focus();

  // The form's `hx-post` answers `HX-Trigger: {"closeModal":…,"showToast":…}`
  // with the re-rendered tab for `#content`: htmx fires the trigger events,
  // THEN swaps — replacing the trigger and the dialog — then afterRequest.
  page.htmx.beforeRequest(submit);
  page.htmx.trigger(submit, 'closeModal', { id: 'create-role' });
  page.htmx.trigger(submit, 'showToast', { message: 'Role created', type: 'success' });
  for (const child of [...content.children]) content.removeChild(child);
  const newTrigger = page.el('button', { 'data-action': 'modal-open', 'data-modal-target': 'create-role' });
  content.appendChild(newTrigger);
  content.appendChild(page.modal('create-role'));
  page.htmx.afterSwap(content);
  page.htmx.afterRequest(submit);
  await tick();

  assert.equal(dialog.open, false);
  assert.equal(page.doc.activeElement, newTrigger, 'focus is on the replacement trigger');
  assert.equal(page.toastContainer.children.length, 1, 'and the toast was shown');
});

test('an open modal swapped out of the document hands focus to the replacement trigger', async () => {
  const page = loadChromeDom();
  const { content, trigger, submit } = pageWithModal(page, 'create-var');
  trigger.focus();
  page.click(trigger);
  submit.focus();

  // Add Variable's form targets `#content` and answers the whole settings
  // body: the open dialog is simply replaced, with no closeModal and no
  // `close` event.
  page.htmx.beforeRequest(submit);
  for (const child of [...content.children]) content.removeChild(child);
  const newTrigger = page.el('button', { 'data-action': 'modal-open', 'data-modal-target': 'create-var' });
  const newDialog = page.modal('create-var');
  content.appendChild(newTrigger);
  content.appendChild(newDialog);
  page.htmx.afterSwap(content);
  page.htmx.afterRequest(submit);
  await tick();

  assert.equal(page.doc.activeElement, newTrigger);
  // The new trigger opens the new dialog: no stale state from the old one.
  page.click(newTrigger);
  assert.equal(newDialog.open, true);
});

test('a field error re-rendered inside the open modal leaves it open and focus where htmx put it', async () => {
  const page = loadChromeDom();
  const { trigger, dialog, submit } = pageWithModal(page, 'create-var');
  page.click(trigger);
  const form = dialog.querySelector('form');

  // `HX-Retarget: #create-var-form`, `HX-Reswap: outerHTML`: the form is
  // replaced, inside the dialog, and the invalid field takes focus.
  page.htmx.beforeRequest(submit);
  const keyField = page.el('input', { id: 'var-key', 'aria-invalid': 'true', autofocus: '' });
  const newForm = page.el('form', {}, [keyField]);
  form.parentNode.appendChild(newForm);
  form.remove();
  keyField.focus();
  page.htmx.afterSwap(newForm);
  page.htmx.afterRequest(submit);
  await tick();

  assert.equal(dialog.open, true);
  assert.equal(page.doc.activeElement, keyField);
});

test('a later, unrelated swap does not pull focus back to an old opener', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  trigger.focus();
  page.click(trigger);
  page.click(dialog.querySelector('.modal__close'));
  await tick();
  assert.equal(page.doc.activeElement, trigger);

  // Somewhere else on the page, a request swaps the region that holds focus.
  const other = page.el('div', { id: 'other' });
  const link = page.el('a', { href: '#' });
  other.appendChild(link);
  page.body.appendChild(other);
  link.focus();
  page.htmx.beforeRequest(link);
  other.removeChild(link);
  page.htmx.afterSwap(other);
  page.htmx.afterRequest(link);

  assert.notEqual(page.doc.activeElement, trigger);
});

test('a script opens a modal through openModal with its own opener', async () => {
  const page = loadChromeDom();
  // files-browser.js: a kebab menu item (about to be removed) opens the
  // share modal, and names the kebab button as where focus goes back.
  const kebab = page.el('button', { class: 'kebab-trigger' });
  const dialog = page.modal('share-link', [page.el('select', { autofocus: '' })]);
  page.body.appendChild(kebab);
  page.body.appendChild(dialog);

  page.body.dispatchEvent(
    page.event('openModal', { bubbles: false, detail: { id: 'share-link', opener: kebab } })
  );
  assert.equal(dialog.open, true);

  dialog.close();
  await tick();
  assert.equal(page.doc.activeElement, kebab);
});

test('while a modal is open, toasts live inside it; they go back to <body> when it closes', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  assert.equal(page.toastContainer.parentNode, page.body);
  assert.equal(page.toastContainer.getAttribute('role'), 'status');

  page.click(trigger);
  assert.equal(page.toastContainer.parentNode, dialog, 'out of the inert page, into the modal');

  // A refusal of the modal's form: an error toast, announced as an alert.
  page.htmx.trigger(null, 'showToast', { message: 'Role exists', type: 'error' });
  const toast = page.toastContainer.children.at(-1);
  assert.equal(toast.getAttribute('role'), 'alert');

  // Its × dismisses the toast and only the toast.
  page.click(toast.children[1]);
  assert.equal(toast.parentNode, null, 'the toast is gone');
  assert.equal(dialog.open, true, 'the modal is still open');

  dialog.close();
  await tick();
  assert.equal(page.toastContainer.parentNode, page.body);
});

test('a non-error toast does not interrupt: it relies on the polite status region', () => {
  const page = loadChromeDom();
  page.htmx.trigger(null, 'showToast', { message: 'Saved', type: 'success' });
  assert.equal(page.toastContainer.children.at(-1).getAttribute('role'), null);
});

test('a toast raised by the response that swaps the modal away survives the swap', async () => {
  const page = loadChromeDom();
  const { content, trigger, submit } = pageWithModal(page, 'create-var');
  page.click(trigger);
  submit.focus();

  // Add Variable: the answer replaces `#content`, which holds the open modal
  // and so the toast container; its `showToast` arrives before the swap.
  page.htmx.beforeRequest(submit);
  page.htmx.trigger(submit, 'showToast', { message: 'Variable created', type: 'success' });
  for (const child of [...content.children]) content.removeChild(child);
  content.appendChild(page.el('button', { 'data-action': 'modal-open', 'data-modal-target': 'create-var' }));
  page.htmx.afterSwap(content);
  page.htmx.afterRequest(submit);
  await tick();

  assert.equal(page.toastContainer.isConnected, true);
  assert.equal(page.toastContainer.parentNode, page.body);
  assert.equal(page.toastContainer.children.length, 1);
});

test('closeModal moves the toasts out at once, so the swap that follows cannot take them', () => {
  const page = loadChromeDom();
  const { content, trigger, submit } = pageWithModal(page);
  page.click(trigger);

  page.htmx.trigger(submit, 'showToast', { message: 'Role created', type: 'success' });
  page.htmx.trigger(submit, 'closeModal', { id: 'create-role' });

  assert.equal(page.toastContainer.parentNode, page.body);
  assert.equal(content.contains(page.toastContainer), false);
});

test('after an htmx-loaded modal saves, focus returns to the re-rendered opener with the same id', async () => {
  const page = loadChromeDom();
  // Edit Variable: the row's Edit button (an `hx-get`, not a modal-open
  // trigger) opens it; Save re-renders `#content`, row button and all.
  const content = page.el('div', { id: 'content-region' });
  const edit = page.el('button', { id: 'edit-var-open-APP', 'hx-get': '/b/admin/variables/APP/edit' });
  const slot = page.el('div', { id: 'edit-var-slot' });
  content.appendChild(edit);
  content.appendChild(slot);
  page.body.appendChild(content);
  edit.focus();
  page.htmx.beforeRequest(edit);
  const save = page.el('button', { type: 'submit' });
  const dialog = page.modal('edit-var', [page.el('form', {}, [save])]);
  slot.appendChild(dialog);
  page.htmx.trigger(edit, 'openModal', { id: 'edit-var' });
  save.focus();

  page.htmx.beforeRequest(save);
  for (const child of [...content.children]) content.removeChild(child);
  const again = page.el('button', { id: 'edit-var-open-APP', 'hx-get': '/b/admin/variables/APP/edit' });
  content.appendChild(again);
  content.appendChild(page.el('div', { id: 'edit-var-slot' }));
  page.htmx.afterSwap(content);
  page.htmx.afterRequest(save);
  await tick();

  assert.equal(page.doc.activeElement, again);
});

test('with no opener left to find, focus goes to main#content rather than nowhere', async () => {
  const page = loadChromeDom();
  const main = page.el('main', { id: 'content', tabindex: '-1' });
  page.body.appendChild(main);
  // A block card: an `hx-get` div with no id that survives, and no trigger.
  const card = page.el('div', { 'hx-get': '/b/admin/blocks/x/detail' });
  main.appendChild(card);
  page.htmx.beforeRequest(card);
  const toggle = page.el('input', { type: 'checkbox' });
  const dialog = page.modal('block-detail', [toggle]);
  main.appendChild(dialog);
  page.htmx.trigger(card, 'openModal', { id: 'block-detail' });

  // The toggle re-renders `#content`'s children, the modal included.
  page.htmx.beforeRequest(toggle);
  for (const child of [...main.children]) main.removeChild(child);
  page.htmx.afterSwap(main);
  page.htmx.afterRequest(toggle);
  await tick();

  assert.equal(page.doc.activeElement, main);
});

test('Tab stops once per radio group, skips tabindex=-1 links, and stops on a details summary', () => {
  const page = loadChromeDom();
  const first = page.el('input', { type: 'radio', name: 'scope', value: 'all', autofocus: '' });
  const second = page.el('input', { type: 'radio', name: 'scope', value: 'one', checked: '' });
  const third = page.el('input', { type: 'radio', name: 'scope', value: 'none' });
  const skipped = page.el('a', { href: '#', tabindex: '-1' });
  const summary = page.el('summary');
  const details = page.el('details', {}, [summary, page.el('p')]);
  const { trigger, dialog } = pageWithModal(page, 'radios', [first, second, third, details, skipped]);
  page.click(trigger);
  const close = dialog.querySelector('.modal__close');

  // Last stop is the summary — the `tabindex="-1"` link after it is none:
  // Tab wraps from it to the close button.
  summary.focus();
  page.key('Tab');
  assert.equal(page.doc.activeElement, close);
  // Shift+Tab from the close button wraps to the summary, not the link.
  page.key('Tab', { shiftKey: true });
  assert.equal(page.doc.activeElement, summary);
});

test('a radio group is one Tab stop: its checked radio', () => {
  const page = loadChromeDom();
  const first = page.el('input', { type: 'radio', name: 'scope', value: 'all' });
  const checked = page.el('input', { type: 'radio', name: 'scope', value: 'one', checked: '' });
  const last = page.el('input', { type: 'radio', name: 'scope', value: 'none' });
  const { trigger, dialog } = pageWithModal(page, 'radios', [first, checked, last]);
  page.click(trigger);

  // The checked radio is the dialog's last stop, so Tab from it wraps.
  checked.focus();
  const e = page.key('Tab');
  assert.equal(e.defaultPrevented, true);
  assert.equal(page.doc.activeElement, dialog.querySelector('.modal__close'));
  // And Shift+Tab from the first stop lands on it, not on the last radio.
  page.key('Tab', { shiftKey: true });
  assert.equal(page.doc.activeElement, checked);
});

test('Esc on an unchanged form closes the modal at once', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  page.click(trigger);

  page.escape(dialog);
  await tick();

  assert.equal(dialog.open, false);
});

test('Esc on a changed form warns first, politely, and closes on the second Esc', async () => {
  const page = loadChromeDom();
  const { trigger, dialog, name } = pageWithModal(page);
  page.click(trigger);
  page.type(name, 'half-typed');

  const first = page.escape(dialog);
  // The note's text lands a moment after the empty status region does.
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.equal(first.defaultPrevented, true);
  assert.equal(dialog.open, true);
  const note = dialog.querySelector('.modal__discard');
  assert.equal(note.getAttribute('role'), 'status');
  assert.equal(note.textContent, 'Press Esc again to discard changes');
  assert.equal(note.parentNode, dialog.querySelector('.modal__footer'), 'shown beside the actions');

  page.escape(dialog);
  await tick();
  assert.equal(dialog.open, false);
  assert.equal(note.textContent, '', 'and the note is gone for the next opening');
});

test('typing after the warning withdraws it: the next Esc warns again', async () => {
  const page = loadChromeDom();
  const { trigger, dialog, name } = pageWithModal(page);
  page.click(trigger);
  page.type(name, 'one');
  page.escape(dialog);
  await new Promise((resolve) => setTimeout(resolve, 80));

  page.type(name, 'one more');
  assert.equal(dialog.querySelector('.modal__discard').textContent, '');
  assert.equal(page.escape(dialog).defaultPrevented, true);
  assert.equal(dialog.open, true);
});

test('Esc on a reference modal (no form) closes it at once, even after a toggle in it changed', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithReferenceModal(page);
  // The block detail's Enabled toggle posts on change: nothing is pending.
  const toggle = page.el('input', { type: 'checkbox', checked: '' });
  dialog.querySelector('.modal__body').appendChild(toggle);
  page.click(trigger);
  toggle.removeAttribute('checked');

  page.escape(dialog);
  await tick();
  assert.equal(dialog.open, false);
});

test('moving the toast container never re-inserts an announced alert', async () => {
  const page = loadChromeDom();
  const { trigger, dialog } = pageWithModal(page);
  page.click(trigger);
  page.htmx.trigger(null, 'showToast', { message: 'Refused', type: 'error' });
  const toast = page.toastContainer.children.at(-1);
  assert.equal(toast.getAttribute('role'), 'alert');

  dialog.close();
  await tick();

  assert.equal(page.toastContainer.parentNode, page.body);
  assert.equal(toast.parentNode, page.toastContainer, 'still shown');
  assert.equal(toast.getAttribute('role'), null, 'but not announced a second time');
});

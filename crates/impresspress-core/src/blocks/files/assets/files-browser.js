// impresspress files-browser bundle.
//
// Bootstrap JSON shape (server-rendered into <script type="application/json">):
//   { "bucket": "photos", "currentPrefix": "nested/" }
//
// All POST URLs match the existing /b/storage/api/* and /b/cloudstorage/* endpoints.
// `showToast` dispatches a `showToast` CustomEvent that the page-level toast handler
// (the "toast notifications" section of ui/assets/chrome.js) listens for.
(function () {
  if (window.__impresspressFilesBrowserInit) return;
  window.__impresspressFilesBrowserInit = true;

  function readBootstrap() {
    const node = document.getElementById('files-browser-bootstrap');
    if (!node) return null;
    try {
      return JSON.parse(node.textContent || '{}');
    } catch (e) {
      return null;
    }
  }

  function showToast(message, type) {
    document.body.dispatchEvent(
      new CustomEvent('showToast', { detail: { message: message, type: type || 'info' } })
    );
  }

  // Upload `files` (a FileList or array of File) to `bucket`/`prefix`.
  // Shared by drag-drop and every "+ Upload" button. The listing is then
  // re-fetched in place rather than the page reloaded, so the outcome stays
  // on screen: the toast, and the bar's live region, say how many went up
  // and name the ones that did not.
  async function uploadFiles(files, bucket, prefix) {
    const list = Array.from(files || []);
    if (list.length === 0) return;
    const failed = [];
    for (const f of list) {
      const key = (prefix || '') + f.name;
      const fd = new FormData();
      fd.append('file', f);
      const url =
        '/b/storage/api/buckets/' +
        encodeURIComponent(bucket) +
        '/objects?key=' +
        encodeURIComponent(key);
      try {
        const resp = await fetch(url, { method: 'POST', body: fd });
        if (!resp.ok) failed.push(f.name);
      } catch (err) {
        failed.push(f.name);
      }
    }
    const done = list.length - failed.length;
    let message = plural(done, 'file') + ' uploaded';
    if (failed.length > 0) {
      message += ', ' + failed.length + " couldn't be uploaded: " + failed.join(', ');
    }
    await refreshListing([]);
    announce(message, failed.length > 0 ? 'error' : 'success');
  }

  function plural(n, noun) {
    return n + ' ' + noun + (n === 1 ? '' : 's');
  }

  // Say an outcome twice over: as a toast, and in the bulk bar's always-
  // present live region when the folder has one.
  function announce(message, type) {
    showToast(message, type);
    const count = document.querySelector('[data-bulk-count]');
    if (count) count.textContent = message;
  }

  // Re-fetch this page and swap its region `id` for the fresh one — the
  // block's one way to show the result of a change without a reload, which
  // would wipe the outcome it reports. `what` names the region in the error
  // if the refresh fails. Every handler here is delegated, so swapped-in
  // markup needs no binding. Resolves to whether the swap happened.
  async function refreshRegion(id, what) {
    const current = document.getElementById(id);
    if (!current) return false;
    try {
      const resp = await fetch(window.location.href, { headers: { Accept: 'text/html' } });
      if (!resp.ok) throw new Error('status ' + resp.status);
      const doc = new DOMParser().parseFromString(await resp.text(), 'text/html');
      const fresh = doc.getElementById(id);
      if (!fresh) throw new Error('no #' + id + ' in the page');
      current.replaceWith(document.importNode(fresh, true));
      return true;
    } catch (e) {
      showToast('The ' + what + " couldn't be refreshed; reload the page to see it.", 'error');
      return false;
    }
  }

  // Refresh the folder's `#object-listing` (the bulk bar and the table,
  // `objects::render_objects_table`), then re-select `keepSelected` — the
  // files an action could not finish with — where they are still listed.
  async function refreshListing(keepSelected) {
    if (!(await refreshRegion('object-listing', 'file list'))) return;
    const keep = new Set(keepSelected);
    document.querySelectorAll('.bulk-select').forEach((box) => {
      box.checked = keep.has(box.dataset.key);
    });
    updateBulkBar();
  }

  // Every destructive action asks first, through the block's one confirm
  // dialog (`pages_user::render_confirm_modal`): `question` in it, Cancel
  // focused, and `action` run when its `data-confirm` button is pressed.
  // `opener` is where focus goes back if it is cancelled.
  let pendingConfirm = null;

  function askToConfirm(dialogId, question, opener, action) {
    const dlg = document.getElementById(dialogId);
    if (!dlg) return;
    pendingConfirm = { dialog: dlg, action: action };
    // textContent, never innerHTML: file names are user-chosen.
    dlg.querySelector('#' + dialogId + '-question').textContent = question;
    document.body.dispatchEvent(
      new CustomEvent('openModal', { detail: { id: dialogId, opener: opener } })
    );
  }

  function confirmButtons() {
    document.addEventListener('click', (e) => {
      const btn = e.target.closest('[data-confirm]');
      if (!btn || !pendingConfirm || !pendingConfirm.dialog.contains(btn)) return;
      const job = pendingConfirm;
      pendingConfirm = null;
      job.dialog.close();
      job.action();
    });
  }

  function dragDropHandler(boot) {
    const root = document.querySelector('.page--list');
    if (!root || !boot.bucket) return;
    const bucket = boot.bucket;
    const prefix = boot.currentPrefix || '';

    root.addEventListener('dragenter', (e) => {
      e.preventDefault();
      root.classList.add('is-drop-target');
    });
    root.addEventListener('dragover', (e) => {
      e.preventDefault();
    });
    root.addEventListener('dragleave', (e) => {
      if (e.target === root) root.classList.remove('is-drop-target');
    });
    root.addEventListener('drop', async (e) => {
      e.preventDefault();
      root.classList.remove('is-drop-target');
      await uploadFiles(e.dataTransfer.files, bucket, prefix);
    });

    // Every "+ Upload" trigger (the topbar's, an empty folder's — which a
    // refresh swaps in) opens the hidden file picker, then uploads what was
    // picked.
    const fileInput = document.getElementById('file-upload-input');
    if (fileInput) {
      document.addEventListener('click', (e) => {
        if (e.target.closest('[data-action="open-upload"]')) fileInput.click();
      });
      fileInput.addEventListener('change', async () => {
        await uploadFiles(fileInput.files, bucket, prefix);
        fileInput.value = '';
      });
    }
  }

  // The bar above the table (`objects::render_bulk_bar`): "Select all
  // files", the selection's count (a live region that is always there), and
  // "Delete selected" once anything is selected. Delegated, because a
  // refresh replaces the bar and the table.
  function bulkSelect() {
    document.addEventListener('change', (e) => {
      if (e.target.matches('[data-bulk-toggle]')) {
        document.querySelectorAll('.bulk-select').forEach((r) => {
          r.checked = e.target.checked;
        });
        updateBulkBar();
      } else if (e.target.matches('.bulk-select')) {
        updateBulkBar();
      }
    });
    document.addEventListener('click', (e) => {
      const del = e.target.closest('[data-bulk-delete]');
      if (del && !del.disabled) askToDelete(selectedKeys(), del);
    });
  }

  function selectedKeys() {
    return Array.from(document.querySelectorAll('.bulk-select:checked'))
      .map((c) => c.dataset.key)
      .filter(Boolean);
  }

  function updateBulkBar() {
    const all = document.querySelector('[data-bulk-toggle]');
    const total = document.querySelectorAll('.bulk-select').length;
    const n = selectedKeys().length;
    if (all) {
      all.checked = total > 0 && n === total;
      all.indeterminate = n > 0 && n < total;
    }
    const del = document.querySelector('[data-bulk-delete]');
    if (del) del.hidden = n === 0;
    const count = document.querySelector('[data-bulk-count]');
    if (count) count.textContent = n === 0 ? '' : plural(n, 'file') + ' selected';
  }

  // A delete — a selection, or one file from its menu — asks first: "Delete
  // 40 files? This can't be undone." While the deletes run, "Delete
  // selected" is disabled and busy, so a second click cannot start them
  // twice.
  let deleting = false;

  function askToDelete(keys, opener) {
    const boot = readBootstrap() || {};
    if (!boot.bucket || !keys.length || deleting) return;
    const bucket = boot.bucket;
    const name = keys.length === 1 ? keys[0].split('/').pop() : plural(keys.length, 'file');
    askToConfirm('delete-confirm', 'Delete ' + name + "? This can't be undone.", opener, () =>
      deleteKeys(bucket, keys, opener)
    );
  }

  async function deleteKeys(bucket, keys, opener) {
    deleting = true;
    const busy = document.querySelector('[data-bulk-delete]');
    if (busy) {
      busy.disabled = true;
      busy.setAttribute('aria-busy', 'true');
    }
    const failed = [];
    for (const key of keys) {
      const url =
        '/b/storage/api/buckets/' +
        encodeURIComponent(bucket) +
        '/objects/' +
        encodeURIComponent(key);
      try {
        const resp = await fetch(url, { method: 'DELETE' });
        if (!resp.ok) failed.push(key);
      } catch (e) {
        failed.push(key);
      }
    }
    // The refresh shows what the bucket now holds; a file that could not be
    // deleted and is still there stays selected, ready to try again.
    await refreshListing(failed);
    deleting = false;
    const deleted = keys.length - failed.length;
    let message = plural(deleted, 'file') + ' deleted';
    if (failed.length > 0) {
      message +=
        ', ' +
        failed.length +
        " couldn't be deleted: " +
        failed.map((k) => k.split('/').pop()).join(', ');
    }
    announce(message, failed.length > 0 ? 'error' : 'success');
    // The control that asked may have gone with the rows it deleted.
    const back = opener && opener.isConnected ? opener : document.querySelector('[data-bulk-toggle]');
    if (back) back.focus();
  }

  // A file row's "more actions" menu. The trigger is a button with
  // `aria-haspopup="menu"`; the menu is a `role="menu"` list of
  // `role="menuitem"` buttons, opened under it and driven from the keyboard
  // the way the ARIA menu-button pattern describes: Enter, Space or
  // ArrowDown open it on the first item (ArrowUp on the last), the arrows,
  // Home and End move between items, Escape closes it and returns focus to
  // the trigger, Tab closes it and lets focus move on.
  let openMenu = null;

  function rowMenus() {
    document.addEventListener('click', (e) => {
      const trigger = e.target.closest('[data-action-menu]');
      if (trigger) {
        e.stopPropagation();
        if (openMenu && openMenu.trigger === trigger) {
          closeMenu(true);
        } else {
          openRowMenu(trigger, 0);
        }
        return;
      }
      if (openMenu && !openMenu.menu.contains(e.target)) closeMenu(false);
    });
    // The menu is placed against the trigger once; it closes rather than
    // drift away from the row when the page scrolls or resizes under it.
    window.addEventListener('resize', () => closeMenu(false));
    document.addEventListener('scroll', () => closeMenu(false), true);
    document.addEventListener('keydown', (e) => {
      const trigger = e.target.closest && e.target.closest('[data-action-menu]');
      if (trigger && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
        e.preventDefault();
        openRowMenu(trigger, e.key === 'ArrowUp' ? -1 : 0);
      }
    });
  }

  function closeMenu(restoreFocus) {
    if (!openMenu) return;
    const { menu, trigger } = openMenu;
    openMenu = null;
    menu.remove();
    trigger.setAttribute('aria-expanded', 'false');
    if (restoreFocus) trigger.focus();
  }

  function menuItem(label, onSelect, danger) {
    const item = document.createElement('button');
    item.type = 'button';
    item.setAttribute('role', 'menuitem');
    item.tabIndex = -1;
    item.className = 'row-menu__item' + (danger ? ' row-menu__item--danger' : '');
    item.textContent = label;
    item.addEventListener('click', () => {
      closeMenu(false);
      onSelect();
    });
    return item;
  }

  function openRowMenu(trigger, focusIndex) {
    closeMenu(false);
    const bucket = trigger.dataset.bucket;
    const key = trigger.dataset.key;
    if (!key) return;
    const menu = document.createElement('div');
    menu.className = 'row-menu';
    menu.setAttribute('role', 'menu');
    menu.setAttribute('aria-label', trigger.getAttribute('aria-label') || 'Actions');
    menu.append(
      menuItem('Share', () => shareModal(bucket, key, trigger)),
      menuItem('Copy link', () => {
        const url =
          window.location.origin +
          '/b/storage/api/buckets/' +
          encodeURIComponent(bucket) +
          '/objects/' +
          encodeURIComponent(key);
        navigator.clipboard.writeText(url);
        showToast('Link copied', 'success');
        trigger.focus();
      }),
      menuItem('Delete', () => askToDelete([key], trigger), true)
    );
    const items = Array.from(menu.querySelectorAll('[role="menuitem"]'));
    menu.addEventListener('keydown', (e) => {
      const at = items.indexOf(document.activeElement);
      let next = null;
      if (e.key === 'ArrowDown') next = (at + 1) % items.length;
      else if (e.key === 'ArrowUp') next = (at - 1 + items.length) % items.length;
      else if (e.key === 'Home') next = 0;
      else if (e.key === 'End') next = items.length - 1;
      else if (e.key === 'Escape') {
        e.preventDefault();
        closeMenu(true);
        return;
      } else if (e.key === 'Tab') {
        // Back on the trigger, and the Tab itself is not stopped: focus moves
        // on from the trigger, as if the menu had never opened.
        closeMenu(true);
        return;
      }
      if (next !== null) {
        e.preventDefault();
        items[next].focus();
      }
    });
    const rect = trigger.getBoundingClientRect();
    menu.style.top = rect.bottom + 'px';
    menu.style.right = window.innerWidth - rect.right + 'px';
    document.body.appendChild(menu);
    trigger.setAttribute('aria-expanded', 'true');
    openMenu = { menu: menu, trigger: trigger };
    items[focusIndex < 0 ? items.length - 1 : focusIndex].focus();
  }

  // A share link's revoke button (`cloudstorage::render_shares_table`): it
  // asks first ("Revoke the link to photos/a.png? Anyone who has it loses
  // access."), then revokes.
  function revokeButtons() {
    document.addEventListener('click', (e) => {
      const trigger = e.target.closest('[data-action="revoke-share"]');
      if (!trigger) return;
      askToConfirm(
        'revoke-confirm',
        'Revoke the link to ' + trigger.dataset.file + '? Anyone who has it loses access.',
        trigger,
        () => revokeShare(trigger.dataset.shareId)
      );
    });
  }

  // `shareId` is the share row's id (rendered as `data-share-id`), which is
  // what DELETE /b/cloudstorage/shares/{id} is keyed on — not the public
  // token in the link. The share list is then refreshed in place and the
  // outcome toasted; focus lands on the list's heading, since the row it was
  // on is gone.
  async function revokeShare(shareId) {
    let revoked = false;
    try {
      const resp = await fetch('/b/cloudstorage/shares/' + encodeURIComponent(shareId), {
        method: 'DELETE',
      });
      revoked = resp.ok;
    } catch (e) {
      revoked = false;
    }
    if (revoked) await refreshRegion('share-listing', 'share list');
    showToast(
      revoked ? 'Share link revoked' : "The share link couldn't be revoked. Try again.",
      revoked ? 'success' : 'error'
    );
    const heading = document.querySelector('#share-listing h2');
    if (heading) {
      heading.tabIndex = -1;
      heading.focus();
    }
  }

  // The share modal is server-rendered next to the object table
  // (`pages_user::objects::render_share_modal`) as the shared
  // `components::modal` <dialog>. This fills in which object it is about and
  // asks chrome.js to open it — through the same `openModal` event the htmx
  // trigger header uses — with the row's menu trigger as the control focus returns
  // to (the menu item that was clicked is gone by the time the modal closes).
  let shareTarget = null;

  function shareModal(bucket, key, opener) {
    const dlg = document.getElementById('share-link');
    if (!dlg) return;
    shareTarget = { bucket: bucket, key: key };
    dlg.querySelector('form').reset();
    // textContent, never innerHTML: bucket and key are user-chosen names.
    dlg.querySelector('#share-object').textContent = bucket + '/' + key;
    document.body.dispatchEvent(
      new CustomEvent('openModal', { detail: { id: 'share-link', opener: opener } })
    );
  }

  function shareForm() {
    const dlg = document.getElementById('share-link');
    if (!dlg) return;
    dlg.querySelector('form').addEventListener('submit', async (e) => {
      e.preventDefault();
      if (!shareTarget) return;
      const bucket = shareTarget.bucket;
      const key = shareTarget.key;
      const hours = dlg.querySelector('select[name="expires"]').value;
      const max = dlg.querySelector('input[name="max"]').value;
      // Only the fields the endpoint declares: it rejects unknown ones
      // rather than minting a share that ignores them. Every option carries
      // an expiry, so one is always sent.
      const body = { bucket: bucket, key: key, expires_in_hours: Number(hours) };
      if (max) body.max_access_count = Number(max);
      try {
        const resp = await fetch('/b/cloudstorage/shares', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });
        if (resp.ok) {
          const json = await resp.json();
          const url = window.location.origin + '/b/storage/direct/' + json.token;
          await navigator.clipboard.writeText(url);
          showToast('Share link copied', 'success');
          dlg.close();
        } else {
          showToast('Share creation failed', 'error');
        }
      } catch (err) {
        showToast('Share creation failed', 'error');
      }
    });
  }

  // S3-style bucket name validation. Rules per AWS S3:
  //   - 3 to 63 characters
  //   - lowercase letters, digits, hyphens; must start and end with letter/digit
  //   - no consecutive hyphens, no `..`
  //   - not formatted as an IP address
  // Returns null when valid, otherwise an error message.
  function validateBucketName(name) {
    if (!name) return 'Bucket name is required.';
    if (name.length < 3 || name.length > 63)
      return 'Bucket name must be 3 to 63 characters.';
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name))
      return 'Use lowercase letters, digits, and hyphens; must start and end with a letter or digit.';
    if (name.indexOf('--') !== -1) return 'Bucket name cannot contain consecutive hyphens.';
    if (name.indexOf('..') !== -1) return 'Bucket name cannot contain consecutive dots.';
    if (/^\d+\.\d+\.\d+\.\d+$/.test(name)) return 'Bucket name cannot look like an IP address.';
    return null;
  }

  // The "New bucket" modal is the shared `components::modal` <dialog>
  // (`pages_user::buckets::render_new_bucket_modal`), opened and closed by
  // chrome.js through `data-action="modal-open"` / `"modal-close"`. What is
  // left here is what is particular to it: validating the name, POSTing it,
  // and starting the next opening with an empty form.
  function bucketCreateForm() {
    const dlg = document.getElementById('new-bucket');
    if (!dlg) return;

    const form = dlg.querySelector('form');
    const nameInput = dlg.querySelector('input[name="name"]');
    const publicInput = dlg.querySelector('input[name="public"]');
    const errEl = dlg.querySelector('#new-bucket-error');
    const submitBtn = dlg.querySelector('button[type="submit"]');

    function showError(msg) {
      if (!errEl) return;
      errEl.textContent = msg || '';
      errEl.hidden = !msg;
    }

    // However it closed — Cancel, the close button, Esc, the backdrop.
    dlg.addEventListener('close', () => {
      form.reset();
      showError('');
      submitBtn.disabled = false;
    });

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      const name = (nameInput.value || '').trim();
      const isPublic = publicInput ? !!publicInput.checked : false;
      const validationError = validateBucketName(name);
      if (validationError) {
        showError(validationError);
        nameInput.focus();
        return;
      }
      submitBtn.disabled = true;
      try {
        const resp = await fetch('/b/storage/api/buckets', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ name: name, public: isPublic }),
        });
        if (resp.ok) {
          // Redirect into the new bucket so the user can immediately upload.
          window.location.href = '/b/storage/' + encodeURIComponent(name) + '/';
          return;
        }
        let serverMsg = 'Failed to create bucket.';
        try {
          const j = await resp.json();
          if (j && j.message) serverMsg = j.message;
        } catch (_) {
          /* ignore JSON parse error */
        }
        showError(serverMsg);
        submitBtn.disabled = false;
      } catch (err) {
        showError('Network error. Please try again.');
        submitBtn.disabled = false;
      }
    });
  }

  window.impresspressFilesBrowser = {
    init: function () {
      const boot = readBootstrap();
      // The row menus and revoke buttons need no bootstrap (the shares page
      // has none).
      rowMenus();
      confirmButtons();
      revokeButtons();
      // The share modal lives on the object list; the bucket-create modal on
      // the bucket lists (no boot bucket). Each binds only where it is.
      shareForm();
      bucketCreateForm();
      if (!boot) return;
      dragDropHandler(boot);
      bulkSelect();

    },
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => window.impresspressFilesBrowser.init());
  } else {
    window.impresspressFilesBrowser.init();
  }
})();

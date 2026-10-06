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
  // Shared by drag-drop and the explicit "+ Upload" button.
  async function uploadFiles(files, bucket, prefix) {
    const list = Array.from(files || []);
    if (list.length === 0) return;
    let successes = 0;
    let failures = 0;
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
        if (resp.ok) {
          successes++;
        } else {
          failures++;
        }
      } catch (err) {
        failures++;
      }
    }
    if (successes > 0) {
      showToast(
        successes + ' uploaded' + (failures > 0 ? ', ' + failures + ' failed' : ''),
        failures > 0 ? 'error' : 'success'
      );
    } else {
      showToast(failures + ' upload failed', 'error');
    }
    window.location.reload();
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

    // Every "+ Upload" trigger (the topbar's, an empty folder's) opens the
    // hidden file picker, then uploads what was picked.
    const fileInput = document.getElementById('file-upload-input');
    if (fileInput) {
      document.querySelectorAll('[data-action="open-upload"]').forEach((trigger) => {
        trigger.addEventListener('click', () => fileInput.click());
      });
      fileInput.addEventListener('change', async () => {
        await uploadFiles(fileInput.files, bucket, prefix);
      });
    }
  }

  // The bar above the table (`objects::render_bulk_bar`): "Select all
  // files", and the count and bulk delete once anything is selected.
  function bulkSelect() {
    const all = document.querySelector('[data-bulk-toggle]');
    if (!all) return;
    const rows = document.querySelectorAll('.bulk-select');
    all.addEventListener('change', () => {
      rows.forEach((r) => {
        r.checked = all.checked;
      });
      updateBulkBar();
    });
    rows.forEach((r) => r.addEventListener('change', updateBulkBar));
    const del = document.querySelector('[data-bulk-delete]');
    if (del) del.addEventListener('click', bulkDelete);
  }

  function selectedKeys() {
    return Array.from(document.querySelectorAll('.bulk-select:checked'))
      .map((c) => c.dataset.key)
      .filter(Boolean);
  }

  function updateBulkBar() {
    const bar = document.getElementById('bulk-action-bar');
    const all = document.querySelector('[data-bulk-toggle]');
    const total = document.querySelectorAll('.bulk-select').length;
    const n = selectedKeys().length;
    if (all) {
      all.checked = total > 0 && n === total;
      all.indeterminate = n > 0 && n < total;
    }
    if (!bar) return;
    bar.hidden = n === 0;
    const count = bar.querySelector('[data-bulk-count]');
    if (count) count.textContent = n + (n === 1 ? ' file selected' : ' files selected');
  }

  async function bulkDelete() {
    const boot = readBootstrap() || {};
    const bucket = boot.bucket;
    const keys = selectedKeys();
    if (!bucket || !keys.length) return;
    if (!window.confirm('Delete ' + keys.length + ' file(s)?')) return;
    let failures = 0;
    for (const key of keys) {
      const url =
        '/b/storage/api/buckets/' +
        encodeURIComponent(bucket) +
        '/objects/' +
        encodeURIComponent(key);
      try {
        const resp = await fetch(url, { method: 'DELETE' });
        if (!resp.ok) failures++;
      } catch (e) {
        failures++;
      }
    }
    showToast(
      keys.length - failures + ' deleted' + (failures > 0 ? ', ' + failures + ' failed' : ''),
      failures > 0 ? 'error' : 'success'
    );
    window.location.reload();
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
      menuItem('Delete', () => confirmDelete(bucket, key, trigger), true)
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
        closeMenu(false);
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

  // A share link's revoke button (`cloudstorage::render_shares_table`).
  function revokeButtons() {
    document.addEventListener('click', (e) => {
      const trigger = e.target.closest('[data-action="revoke-share"]');
      if (trigger) revokeShare(trigger.dataset.shareId);
    });
  }

  // `shareId` is the share row's id (rendered as `data-share-id`), which is
  // what DELETE /b/cloudstorage/shares/{id} is keyed on — not the public
  // token in the link.
  async function revokeShare(shareId) {
    if (!window.confirm('Revoke this share link?')) return;
    try {
      const resp = await fetch('/b/cloudstorage/shares/' + encodeURIComponent(shareId), {
        method: 'DELETE',
      });
      if (resp.ok) {
        showToast('Share revoked', 'success');
        window.location.reload();
      } else {
        showToast('Revoke failed', 'error');
      }
    } catch (e) {
      showToast('Revoke failed', 'error');
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

  async function confirmDelete(bucket, key, trigger) {
    if (!window.confirm('Delete ' + key + '?')) {
      trigger.focus();
      return;
    }
    const url =
      '/b/storage/api/buckets/' +
      encodeURIComponent(bucket) +
      '/objects/' +
      encodeURIComponent(key);
    try {
      const resp = await fetch(url, { method: 'DELETE' });
      if (resp.ok) {
        showToast('Deleted', 'success');
        window.location.reload();
      } else {
        showToast('Delete failed', 'error');
      }
    } catch (e) {
      showToast('Delete failed', 'error');
    }
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

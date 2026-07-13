// impresspress files-browser bundle.
//
// Bootstrap JSON shape (server-rendered into <script type="application/json">):
//   { "bucket": "photos", "currentPrefix": "nested/" }
//
// All POST URLs match the existing /b/storage/api/* and /b/cloudstorage/* endpoints.
// `showToast` dispatches a `showToast` CustomEvent that the page-level toast handler
// (in ui/assets.rs::toast_js) listens for.
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

    // Topbar Upload button: trigger the hidden file picker, then upload.
    const trigger = document.querySelector('[data-action="open-upload"]');
    const fileInput = document.getElementById('file-upload-input');
    if (trigger && fileInput) {
      trigger.addEventListener('click', () => fileInput.click());
      fileInput.addEventListener('change', async () => {
        await uploadFiles(fileInput.files, bucket, prefix);
      });
    }
  }

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
  }

  function selectedKeys() {
    return Array.from(document.querySelectorAll('.bulk-select:checked'))
      .map((c) => c.dataset.key)
      .filter(Boolean);
  }

  function updateBulkBar() {
    let bar = document.getElementById('bulk-action-bar');
    const keys = selectedKeys();
    if (!bar) {
      bar = document.createElement('div');
      bar.id = 'bulk-action-bar';
      bar.className = 'bulk-action-bar';
      bar.innerHTML = '<button type="button" data-bulk-delete>Delete selected</button>';
      const target = document.querySelector('.page--list .page-body');
      if (target) target.prepend(bar);
      bar.querySelector('[data-bulk-delete]').addEventListener('click', bulkDelete);
    }
    bar.style.display = keys.length > 0 ? '' : 'none';
    bar.dataset.count = String(keys.length);
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

  function kebabMenu() {
    document.addEventListener('click', (e) => {
      const trigger = e.target.closest('[data-action-menu]');
      if (trigger) {
        e.stopPropagation();
        openKebab(trigger);
        return;
      }
      closeAllKebabs();
    });
  }

  function closeAllKebabs() {
    document.querySelectorAll('.kebab-popup').forEach((p) => p.remove());
  }

  function openKebab(trigger) {
    closeAllKebabs();
    const popup = document.createElement('div');
    popup.className = 'kebab-popup';
    if (trigger.dataset.token) {
      // Shares table kebab.
      popup.innerHTML = '<button type="button" data-action="revoke">Revoke share</button>';
      popup.querySelector('[data-action="revoke"]').addEventListener('click', () => {
        revokeShare(trigger.dataset.token);
      });
    } else if (trigger.dataset.key) {
      // Object table kebab.
      popup.innerHTML =
        '<button type="button" data-action="share">Share</button>' +
        '<button type="button" data-action="copy">Copy link</button>' +
        '<button type="button" data-action="delete">Delete</button>';
      popup.querySelector('[data-action="share"]').addEventListener('click', () => {
        shareModal(trigger.dataset.bucket, trigger.dataset.key);
      });
      popup.querySelector('[data-action="copy"]').addEventListener('click', () => {
        const url =
          window.location.origin +
          '/b/storage/api/buckets/' +
          encodeURIComponent(trigger.dataset.bucket) +
          '/objects/' +
          encodeURIComponent(trigger.dataset.key);
        navigator.clipboard.writeText(url);
        showToast('Link copied', 'success');
      });
      popup.querySelector('[data-action="delete"]').addEventListener('click', () => {
        confirmDelete(trigger.dataset.bucket, trigger.dataset.key);
      });
    }
    const rect = trigger.getBoundingClientRect();
    popup.style.position = 'fixed';
    popup.style.top = rect.bottom + 'px';
    popup.style.right = window.innerWidth - rect.right + 'px';
    document.body.appendChild(popup);
  }

  async function revokeShare(token) {
    if (!window.confirm('Revoke this share link?')) return;
    try {
      const resp = await fetch('/b/cloudstorage/shares/' + encodeURIComponent(token), {
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

  function shareModal(bucket, key) {
    const dlg = document.createElement('dialog');
    dlg.className = 'share-modal';
    dlg.innerHTML =
      '<form method="dialog">' +
      '<h3>Create share link</h3>' +
      '<p><code></code></p>' +
      '<label>Expires in <select name="expires">' +
      '<option value="">Never</option>' +
      '<option value="1">1 day</option>' +
      '<option value="7" selected>7 days</option>' +
      '<option value="30">30 days</option>' +
      '</select></label>' +
      '<label>Max accesses <input name="max" type="number" min="0" placeholder="∞" /></label>' +
      '<div class="modal-actions">' +
      '<button type="button" data-action="cancel">Cancel</button>' +
      '<button type="button" data-action="create">Create</button>' +
      '</div>' +
      '</form>';
    // Set the source code element via textContent (avoids innerHTML XSS on bucket/key).
    dlg.querySelector('code').textContent = bucket + '/' + key;
    document.body.appendChild(dlg);
    dlg.showModal();
    dlg.querySelector('[data-action="cancel"]').addEventListener('click', () => {
      dlg.close();
      dlg.remove();
    });
    dlg.querySelector('[data-action="create"]').addEventListener('click', async () => {
      const days = dlg.querySelector('select[name="expires"]').value;
      const max = dlg.querySelector('input[name="max"]').value;
      const body = { bucket: bucket, key: key };
      if (days) body.expires_days = Number(days);
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
          dlg.remove();
        } else {
          showToast('Share creation failed', 'error');
        }
      } catch (e) {
        showToast('Share creation failed', 'error');
      }
    });
  }

  async function confirmDelete(bucket, key) {
    if (!window.confirm('Delete ' + key + '?')) return;
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

  function bucketCreateModal() {
    const trigger = document.querySelector('[data-action="open-new-bucket"]');
    const dlg = document.getElementById('new-bucket-modal');
    if (!trigger || !dlg) return;

    const form = dlg.querySelector('form');
    const nameInput = dlg.querySelector('input[name="name"]');
    const publicInput = dlg.querySelector('input[name="public"]');
    const errEl = dlg.querySelector('.modal-error');
    const cancelBtn = dlg.querySelector('[data-action="cancel"]');
    const submitBtn = dlg.querySelector('[data-action="create"]');

    function showError(msg) {
      if (!errEl) return;
      errEl.textContent = msg || '';
      errEl.hidden = !msg;
    }

    function resetForm() {
      if (form) form.reset();
      showError('');
    }

    trigger.addEventListener('click', (e) => {
      e.preventDefault();
      resetForm();
      dlg.showModal();
      // Focus the name input on open.
      if (nameInput) nameInput.focus();
    });

    cancelBtn.addEventListener('click', () => {
      dlg.close();
    });

    // Native <dialog> already handles ESC-to-close; clear error on close.
    dlg.addEventListener('close', resetForm);

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
      // shareModal/kebab still useful even without bootstrap (e.g., shares page).
      kebabMenu();
      // bucket-create modal lives on the bucket-list page (no boot bucket).
      bucketCreateModal();
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

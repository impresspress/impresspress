// The legal document editor (`blocks/legalpages/pages.rs`, `editor_view`).
//
// Inlined into the editor page, which is a `ui::shell_page`: reaching it can
// be an htmx partial swap that re-executes this script against a `document`
// that outlived the previous page, so the listeners are registered once per
// document. They are delegated, and every one of them first finds the editor
// (`#legal-editor`) and does nothing when the page on screen is not the
// editor — the document-level Ctrl/Cmd+S above all, which outlives the
// editor and must leave the browser's own shortcut alone everywhere else.
(function() {
    if (window.__legalpagesEditorInit) return;
    window.__legalpagesEditorInit = true;

    // The editor on screen, or null. The started check is the empty-state
    // case: before "Write …" is pressed there is nothing to save.
    function editorRoot() {
        var root = document.getElementById('legal-editor');
        return root && !root.hidden ? root : null;
    }

    function toast(message, type) {
        document.body.dispatchEvent(new CustomEvent('showToast', {
            detail: { message: message, type: type }
        }));
    }

    // --- Edit / Preview tabs (WAI-ARIA tabs, automatic activation) ---------

    function tabs(root) {
        return Array.prototype.slice.call(root.querySelectorAll('[role="tab"]'));
    }

    function selectTab(root, tab, focus) {
        tabs(root).forEach(function(t) {
            var selected = t === tab;
            t.setAttribute('aria-selected', selected ? 'true' : 'false');
            t.tabIndex = selected ? 0 : -1;
            t.classList.toggle('editor-tab--active', selected);
            document.getElementById(t.getAttribute('aria-controls')).hidden = !selected;
        });
        if (focus) tab.focus();
        if (tab.dataset.tab === 'preview') renderPreview(root);
    }

    function renderPreview(root) {
        var target = document.getElementById('editor-preview');
        target.setAttribute('aria-busy', 'true');
        fetch(root.dataset.previewUrl, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ content: document.getElementById('editor').value })
        })
        .then(function(r) {
            if (!r.ok) throw new Error('HTTP ' + r.status);
            return r.text();
        })
        .then(function(html) { target.innerHTML = html; })
        .catch(function(err) {
            target.innerHTML = '';
            var p = document.createElement('p');
            p.className = 'text-danger';
            p.setAttribute('role', 'alert');
            p.textContent = 'Preview failed: ' + err.message;
            target.appendChild(p);
        })
        .finally(function() { target.removeAttribute('aria-busy'); });
    }

    // --- Empty state: start the first version --------------------------------

    function start() {
        var root = document.getElementById('legal-editor');
        if (!root) return;
        root.hidden = false;
        var empty = document.getElementById('legal-editor-empty');
        if (empty) empty.hidden = true;
        document.querySelectorAll('[data-legal-editor-action]').forEach(function(el) {
            el.hidden = false;
        });
        document.getElementById('title-input').focus();
    }

    // --- Save draft / publish ------------------------------------------------

    function setStatus(status) {
        var badge = document.querySelector('#document-status .badge');
        badge.className = 'badge ' + (status === 'published' ? 'badge-success' : 'badge-warning');
        badge.textContent = status === 'published' ? 'Published' : 'Draft';
    }

    function saveDocument(root, publish) {
        var btn = document.getElementById(publish ? 'btn-publish' : 'btn-save');
        if (!btn || btn.disabled) return;
        var label = btn.textContent;
        btn.disabled = true;
        btn.setAttribute('aria-busy', 'true');
        btn.textContent = publish ? 'Publishing…' : 'Saving…';

        fetch(publish ? root.dataset.publishUrl : root.dataset.saveUrl, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                doc_type: root.dataset.docType,
                doc_id: root.dataset.docId,
                title: document.getElementById('title-input').value,
                content: document.getElementById('editor').value
            })
        })
        .then(function(r) {
            return r.json().then(function(data) { return { ok: r.ok, status: r.status, data: data }; },
                                 function() { return { ok: r.ok, status: r.status, data: {} }; });
        })
        .then(function(res) {
            var data = res.data;
            if (!res.ok) {
                toast(data.message || ('Saving failed (HTTP ' + res.status + ')'), 'error');
                return;
            }
            if (data.doc_id) root.dataset.docId = data.doc_id;
            if (data.status) setStatus(data.status);
            if (data.version) {
                // The server numbered it; the next publish takes the one after.
                document.getElementById('live-version').textContent = 'Live: v' + data.version;
                document.getElementById('next-version').textContent = 'Publishes as v' + (data.version + 1);
            }
            var saved = document.getElementById('saved-at');
            if (saved) saved.textContent = publish ? 'Published just now' : 'Saved just now';
            toast(data.message || 'Saved', 'success');
        })
        .catch(function(err) { toast('Saving failed: ' + err.message, 'error'); })
        .finally(function() {
            btn.disabled = false;
            btn.removeAttribute('aria-busy');
            btn.textContent = label;
        });
    }

    // --- Listeners -----------------------------------------------------------

    document.addEventListener('click', function(e) {
        if (!(e.target instanceof Element)) return;
        var el = e.target.closest('[data-action^="legalpages-"]');
        if (!el) return;
        var action = el.getAttribute('data-action');
        if (action === 'legalpages-start') { start(); return; }
        var root = editorRoot();
        if (!root) return;
        if (action === 'legalpages-editor-tab') selectTab(root, el, false);
        else if (action === 'legalpages-save') saveDocument(root, false);
        else if (action === 'legalpages-publish') saveDocument(root, true);
    });

    document.addEventListener('keydown', function(e) {
        var root = editorRoot();
        if (!root) return;

        // Ctrl+S / Cmd+S saves a draft — only while the editor is on screen.
        if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && (e.key === 's' || e.key === 'S')) {
            e.preventDefault();
            saveDocument(root, false);
            return;
        }

        // Arrow keys, Home and End move between the Edit and Preview tabs.
        if (!(e.target instanceof Element) || e.target.getAttribute('role') !== 'tab'
            || !root.contains(e.target)) return;
        var all = tabs(root);
        var i = all.indexOf(e.target);
        var next = null;
        if (e.key === 'ArrowRight') next = all[(i + 1) % all.length];
        else if (e.key === 'ArrowLeft') next = all[(i - 1 + all.length) % all.length];
        else if (e.key === 'Home') next = all[0];
        else if (e.key === 'End') next = all[all.length - 1];
        if (!next) return;
        e.preventDefault();
        selectTab(root, next, true);
    });
})();

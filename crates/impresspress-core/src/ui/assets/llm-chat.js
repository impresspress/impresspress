// Impresspress LLM chat — extracted from inline SHARED_JS/CHAT_JS/THREAD_JS.
// Entry point: impresspressLlmChat.init() — reads initial server-rendered
// messages from <script type="application/json" id="llm-chat-bootstrap">.
// All globals previously exposed (handleChatSubmit, selectThread, createNewThread,
// onModelChange, unloadLocalModel) remain on window so existing onclick="" /
// onsubmit="" attributes keep working.

(function () {
  if (window.__impresspressLlmChatLoaded) return;
  window.__impresspressLlmChatLoaded = true;

  // -------------------------------------------------------------------------
  // Markdown rendering
  // -------------------------------------------------------------------------

  function renderMarkdown(text) {
    if (typeof marked !== 'undefined' && marked.parse) {
      try {
        var html = marked.parse(text, { breaks: true });
        if (typeof DOMPurify !== 'undefined') {
          return DOMPurify.sanitize(html);
        }
        // No sanitizer available → do not emit raw HTML.
        return escHtml(text).replace(/\n/g, '<br>');
      } catch (e) {}
    }
    return escHtml(text).replace(/\n/g, '<br>');
  }

  // Scroll the chat pane to the newest message. The scroll container is the
  // chat_page template's `.chat-messages` wrapper — #messages-area is just
  // the htmx/JS insertion target inside it (no overflow of its own).
  function scrollChatToBottom() {
    var area = document.getElementById('messages-area');
    if (!area) return;
    var pane = area.closest('.chat-messages') || area;
    pane.scrollTop = pane.scrollHeight;
  }

  function escHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  // -------------------------------------------------------------------------
  // Message card rendering
  // -------------------------------------------------------------------------

  function messageCardHtml(role, content, date, opts) {
    opts = opts || {};
    var isMarkdown = (role === 'assistant');
    var rendered = isMarkdown ? renderMarkdown(content) : escHtml(content);

    // Brand-consistent card accents — keep in sync with `entry_card` in
    // blocks/messages/pages.rs (the SSR renderer of the same cards): user =
    // brand tint, assistant = neutral, system = warning yellow.
    var bg, badge;
    if (role === 'user') {
      bg = 'background:#fff1e6;border-left:3px solid var(--primary-color)';
      badge = 'badge';
    } else if (role === 'assistant') {
      bg = 'background:var(--surface-3);border-left:3px solid var(--border-color)';
      badge = 'badge';
    } else if (role === 'system') {
      bg = 'background:#fefce8;border-left:3px solid #eab308';
      badge = 'badge-warning';
    } else {
      bg = 'background:var(--surface-3);border-left:3px solid var(--border-color)';
      badge = 'badge';
    }

    var modelBadge = '';
    if (opts.model) {
      modelBadge = ' <span class="badge" style="font-size:0.7rem">' + escHtml(opts.model) + '</span>';
    }

    var contentStyle = isMarkdown
      ? 'margin:0;word-break:break-word;line-height:1.6'
      : 'margin:0;white-space:pre-wrap;word-break:break-word';

    var id = opts.id ? ' id="' + opts.id + '"' : '';

    return '<div class="card"' + id + ' style="margin-bottom:0.75rem;' + bg + '">'
      + '<div style="display:flex;align-items:center;gap:0.5rem;margin-bottom:0.5rem">'
      + '<span class="badge ' + badge + '" style="text-transform:capitalize">' + role + '</span>'
      + (date ? '<span class="text-muted" style="font-size:0.75rem">' + escHtml(date) + '</span>' : '')
      + modelBadge
      + '</div>'
      + '<div style="' + contentStyle + '">' + rendered + '</div>'
      + '</div>';
  }

  function appendMessageCard(role, content, opts) {
    var area = document.getElementById('messages-area');
    if (!area) return null;
    var placeholder = area.querySelector('.text-center.text-muted');
    if (placeholder) placeholder.remove();

    var wrapper = document.createElement('div');
    var date = new Date().toISOString().slice(0, 10);
    wrapper.innerHTML = messageCardHtml(role, content, date, opts);
    var card = wrapper.firstChild;
    area.appendChild(card);
    scrollChatToBottom();
    return card;
  }

  // -------------------------------------------------------------------------
  // Local model management
  // -------------------------------------------------------------------------

  var _localModelLoading = false;

  async function populateLocalModels() {
    if (!window.impresspressAI) return;
    var status = window.impresspressAI.getStatus();
    if (!status.webgpu_supported) {
      var group = document.getElementById('local-models-group');
      if (group) group.label = 'Local (WebGPU not available)';
      return;
    }
    var models = await window.impresspressAI.getAvailableModels();
    var group = document.getElementById('local-models-group');
    if (!group) return;
    group.innerHTML = '';
    models.forEach(function (m) {
      var opt = document.createElement('option');
      opt.value = 'local:' + m.id;
      opt.textContent = m.name;
      group.appendChild(opt);
    });
  }

  function onModelChange(value) {
    if (value && value.startsWith('local:')) {
      var modelId = value.slice(6);
      loadLocalModel(modelId);
    } else {
      updateModelStatus('');
    }
  }

  function loadLocalModel(modelId) {
    if (!window.impresspressAI) {
      updateModelStatus('WebLLM not loaded yet. Wait for page to finish loading.');
      return;
    }
    var status = window.impresspressAI.getStatus();
    if (status.loaded_model === modelId) {
      updateModelStatus('Ready');
      return;
    }

    _localModelLoading = true;
    showModelProgress(true);
    updateModelStatus('Loading...');

    window.impresspressAI.loadModel(modelId, function (progress) {
      var pct = Math.round(progress.progress * 100);
      var bar = document.getElementById('model-progress-bar');
      var text = document.getElementById('model-progress-text');
      if (bar) bar.style.width = pct + '%';
      if (text) text.textContent = progress.text;
    }).then(function () {
      _localModelLoading = false;
      showModelProgress(false);
      updateModelStatus('Ready');
    }).catch(function (err) {
      _localModelLoading = false;
      showModelProgress(false);
      updateModelStatus('Error: ' + err.message);
      console.error('[impresspress] Model load error:', err);
    });
  }

  function unloadLocalModel() {
    if (!window.impresspressAI) return;
    window.impresspressAI.unloadModel().then(function () {
      _localModelLoading = false;
      showModelProgress(false);
      updateModelStatus('');
      var picker = document.getElementById('model-picker');
      if (picker) picker.value = '';
    });
  }

  function showModelProgress(show) {
    var container = document.getElementById('model-progress-container');
    if (container) container.style.display = show ? 'block' : 'none';
  }

  function updateModelStatus(text) {
    var el = document.getElementById('model-status');
    if (el) el.textContent = text;
  }

  // -------------------------------------------------------------------------
  // Chat submission
  // -------------------------------------------------------------------------

  var _chatBusy = false;

  function handleChatSubmit(e) {
    e.preventDefault();
    if (_chatBusy) return false;

    var form = document.getElementById('chat-form');
    var textarea = document.getElementById('chat-input');
    var threadId = document.getElementById('active-thread-id').value;
    var userText = textarea.value.trim();

    if (!userText || !threadId) return false;

    _chatBusy = true;
    setSendEnabled(false);
    textarea.value = '';

    appendMessageCard('user', userText);

    var picker = document.getElementById('model-picker');
    var model = picker ? picker.value : '';
    // The selected option carries the bare model id as its value and the
    // backend id in data-backend-id, so model + provider go as separate fields
    // (the old "backend:model" composite was ambiguous and mis-sent the model).
    var backendId = (picker && picker.selectedOptions[0])
      ? (picker.selectedOptions[0].dataset.backendId || '')
      : '';

    var chatPromise;
    if (model.startsWith('local:')) {
      chatPromise = fetch('/b/messages/api/contexts/' + threadId + '/entries', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ kind: 'message', role: 'user', content: userText })
      }).then(function () {
        return handleLocalChat(threadId, model.slice(6));
      });
    } else {
      chatPromise = handleRemoteChat(threadId, userText, model, backendId);
    }

    chatPromise.catch(function (err) {
      appendMessageCard('system', 'Error: ' + err.message);
    }).finally(function () {
      _chatBusy = false;
      setSendEnabled(true);
    });

    return false;
  }

  function handleLocalChat(threadId, modelId) {
    if (!window.impresspressAI) {
      appendMessageCard('system', 'WebLLM not loaded. Select a local model first.');
      return Promise.resolve();
    }

    return fetch('/b/messages/api/contexts/' + threadId + '/entries?kind=message')
      .then(function (r) { return r.json(); })
      .then(function (data) {
        var records = data.records || [];
        var messages = records.map(function (m) {
          var d = m.data || m;
          return { role: d.role, content: d.content };
        });

        var card = appendMessageCard('assistant', '', { id: 'streaming-msg' });
        var contentDiv = card ? card.querySelector('div:last-child') : null;
        if (contentDiv) contentDiv.innerHTML = '<span class="text-muted" style="animation:pulse 1.5s infinite">Thinking...</span>';
        setSendStatus('AI is thinking...');

        return window.impresspressAI.chat(messages, function (delta, full) {
          setSendStatus('AI is typing...');
          if (contentDiv) {
            contentDiv.innerHTML = renderMarkdown(full) + '<span class="typing-cursor"></span>';
            scrollChatToBottom();
          }
        });
      })
      .then(function (result) {
        var streamCard = document.getElementById('streaming-msg');
        if (streamCard) {
          streamCard.removeAttribute('id');
          var cursor = streamCard.querySelector('.typing-cursor');
          if (cursor) cursor.remove();
          var cd = streamCard.querySelector('div:last-child');
          if (cd && result.content) cd.innerHTML = renderMarkdown(result.content);
        }

        return fetch('/b/messages/api/contexts/' + threadId + '/entries', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ kind: 'message', role: 'assistant', content: result.content })
        });
      });
  }

  function handleRemoteChat(threadId, userText, model, backendId) {
    var card = appendMessageCard('assistant', '', { id: 'streaming-msg' });
    var contentDiv = card ? card.querySelector('div:last-child') : null;
    if (contentDiv) contentDiv.innerHTML = '<span class="text-muted" style="animation:pulse 1.5s infinite">Thinking...</span>';
    setSendStatus('Waiting for response...');

    var body = { thread_id: threadId, message: userText };
    if (model) body.model = model;
    if (backendId) body.provider = backendId;

    return fetch('/b/llm/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    })
    .then(function (r) { return r.json(); })
    .then(function (data) {
      var streamCard = document.getElementById('streaming-msg');
      if (streamCard) {
        var contentDiv = streamCard.querySelector('div:last-child');
        if (contentDiv) {
          contentDiv.innerHTML = renderMarkdown(data.content || 'No response');
          contentDiv.style.margin = '0';
          contentDiv.style.wordBreak = 'break-word';
          contentDiv.style.lineHeight = '1.6';
        }
        if (data.model) {
          var header = streamCard.querySelector('div:first-child');
          if (header) {
            var badge = document.createElement('span');
            badge.className = 'badge';
            badge.style.fontSize = '0.7rem';
            badge.textContent = data.model;
            header.appendChild(badge);
          }
        }
        streamCard.removeAttribute('id');
      }
    })
    .catch(function (err) {
      var streamCard = document.getElementById('streaming-msg');
      if (streamCard) streamCard.remove();
      appendMessageCard('system', 'Error: ' + err.message);
    });
  }

  function setSendEnabled(enabled) {
    var btn = document.getElementById('send-btn');
    var input = document.getElementById('chat-input');
    if (btn) { btn.disabled = !enabled; btn.textContent = enabled ? 'Send' : 'Sending...'; }
    if (input) input.disabled = !enabled;
    if (enabled) setSendStatus('');
  }

  function setSendStatus(text) {
    var el = document.getElementById('send-status');
    if (el) el.textContent = text;
  }

  // -------------------------------------------------------------------------
  // Thread creation + selection
  // -------------------------------------------------------------------------

  function createNewThread() {
    // LLM threads ARE messages contexts of type "conversation" (the thread
    // list is served from the same store). The old `/b/messages/api/threads`
    // endpoint never existed — the + button 404'd silently.
    fetch('/b/messages/api/contexts', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ type: 'conversation', title: 'New Chat' })
    })
    .then(function (r) { return r.json(); })
    .then(function (data) {
      var id = data.id || (data.data && data.data.id);
      if (id) {
        var list = document.getElementById('thread-list');
        if (list) {
          var placeholder = list.querySelector('.text-center.text-muted');
          if (placeholder) placeholder.remove();
          var date = new Date().toISOString().slice(0, 10);
          var html = '<div class="card" style="margin-bottom:0.375rem;cursor:pointer;padding:0.625rem 0.75rem;transition:box-shadow 0.15s" '
            + 'data-thread-id="' + id + '" '
            + 'onclick="selectThread(\'' + id + '\')" '
            + 'onmouseover="this.style.boxShadow=\'0 2px 8px rgba(0,0,0,0.1)\'" '
            + 'onmouseout="this.style.boxShadow=\'\'">'
            + '<div style="display:flex;align-items:center;justify-content:space-between;gap:0.5rem">'
            + '<span style="font-weight:500;font-size:0.875rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:1">New Chat</span>'
            + '<span class="text-muted" style="font-size:0.75rem;flex-shrink:0">' + date + '</span>'
            + '</div></div>';
          list.insertAdjacentHTML('afterbegin', html);
        }
        selectThread(id);
      }
    })
    .catch(function (err) {
      console.error('[impresspress] Error creating thread:', err);
    });
  }

  function selectThread(id) {
    document.getElementById('active-thread-id').value = id;

    var form = document.getElementById('chat-form');
    if (form) {
      form.style.opacity = '1';
      form.style.pointerEvents = 'auto';
    }
    var input = document.getElementById('chat-input');
    if (input) { input.disabled = false; input.placeholder = 'Type your message...'; input.focus(); }
    var btn = document.getElementById('send-btn');
    if (btn) btn.disabled = false;
    var prompt = document.getElementById('no-thread-prompt');
    if (prompt) prompt.remove();

    fetch('/b/messages/api/contexts/' + id + '/entries?kind=message')
      .then(function (r) { return r.json(); })
      .then(function (data) {
        var records = data.records || [];
        var area = document.getElementById('messages-area');
        if (!area) return;

        if (records.length === 0) {
          area.innerHTML = '<div class="text-center text-muted" style="padding:2rem">No messages yet.</div>';
        } else {
          var html = records.map(function (m) {
            var d = m.data || m;
            var role = d.role || 'user';
            var content = d.content || '';
            var date = (d.created_at || '').slice(0, 10);
            return messageCardHtml(role, content, date);
          }).join('');
          area.innerHTML = html;
        }
        scrollChatToBottom();
      })
      .catch(function (err) {
        console.error('[impresspress] Error loading messages:', err);
      });

    // Toggle the same attribute the server renders — the highlight colors
    // live in one CSS rule (`.chat-threads .card[data-active="true"]`), so
    // SSR and client-side switching can't drift.
    document.querySelectorAll('[data-thread-id]').forEach(function (el) {
      el.dataset.active = el.dataset.threadId === id ? 'true' : 'false';
    });

    history.replaceState({}, '', '/b/llm/threads/' + id);
  }

  // -------------------------------------------------------------------------
  // Initial render of pre-loaded thread messages (server-rendered data)
  // -------------------------------------------------------------------------

  function renderInitialMessages() {
    var carrier = document.getElementById('llm-chat-bootstrap');
    var messages = [];
    if (carrier) {
      try {
        messages = JSON.parse(carrier.textContent || '[]');
      } catch (e) {
        console.warn('[impresspress] failed to parse llm-chat-bootstrap:', e);
      }
    }
    var area = document.getElementById('messages-area');
    if (!area || messages.length === 0) return;

    area.innerHTML = messages.map(function (m) {
      var date = (m.created_at || '').slice(0, 10);
      return messageCardHtml(m.role, m.content, date);
    }).join('');
    scrollChatToBottom();
  }

  // -------------------------------------------------------------------------
  // Public init entry point + global re-exports for inline handlers
  // -------------------------------------------------------------------------

  function init() {
    renderInitialMessages();
    setTimeout(populateLocalModels, 1500);
    setTimeout(populateLocalModels, 5000);

    // Desktop fast-path: clicking a thread <a href> in the sidebar performs
    // an in-page swap via selectThread() instead of a full page reload.
    // The href stays as the no-JS / first-paint fallback.
    document.addEventListener('click', function (e) {
      var t = e.target.closest('[data-thread-id]');
      if (!t) return;
      // Only intercept left-clicks without modifier keys (let cmd/ctrl-click
      // open in new tab, middle-click work as expected).
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      var id = t.dataset.threadId;
      if (!id) return;
      e.preventDefault();
      selectThread(id);
    });
  }

  window.impresspressLlmChat = { init: init };
  // Re-export named handlers used by inline onclick="" / onsubmit="" attributes.
  window.handleChatSubmit = handleChatSubmit;
  window.createNewThread = createNewThread;
  window.selectThread = selectThread;
  window.onModelChange = onModelChange;
  window.unloadLocalModel = unloadLocalModel;
})();

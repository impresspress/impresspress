// Impresspress LLM chat — extracted from inline SHARED_JS/CHAT_JS/THREAD_JS.
// Entry point: impresspressLlmChat.init() — reads initial server-rendered
// messages from <script type="application/json" id="llm-chat-bootstrap">.
// Nothing here is exposed on `window` any more. The page's controls declare
// `data-action` verbs (`llm-new-thread`, `llm-model-change`,
// `llm-unload-model`) and the chat form is bound by its id, all read by the
// delegated listeners in `bindPageControls()` at the bottom of this file --
// see the rule written out in `ui/assets/chrome.js`.

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

  // The card classes are the ones `entry_card` in blocks/messages/pages.rs
  // renders for the same cards server-side (components/card.css): user =
  // brand tint, assistant = neutral, system = warning yellow. Built with the
  // DOM API, so nothing here carries an inline style or concatenates text
  // into markup; only an assistant turn's sanitized markdown is HTML.
  var CARD_VARIANT = {
    user: 'message-card--user',
    assistant: 'message-card--neutral',
    system: 'message-card--warning'
  };

  function badge(text, variant) {
    var el = document.createElement('span');
    el.className = 'badge text-capitalize' + (variant ? ' ' + variant : '');
    el.textContent = text;
    return el;
  }

  function messageCard(role, content, date, opts) {
    opts = opts || {};
    var card = document.createElement('div');
    card.className = 'card ' + (CARD_VARIANT[role] || 'message-card--neutral');
    if (opts.id) card.id = opts.id;

    var head = document.createElement('div');
    head.className = 'message-card__head';
    head.appendChild(badge(role, role === 'system' ? 'badge-warning' : ''));
    if (date) {
      var when = document.createElement('span');
      when.className = 'text-muted text-xs';
      when.textContent = date;
      head.appendChild(when);
    }
    if (opts.model) head.appendChild(modelBadge(opts.model));
    card.appendChild(head);

    var body = document.createElement('div');
    if (role === 'assistant') {
      body.className = 'message-card__content message-card__content--markdown';
      body.innerHTML = renderMarkdown(content);
    } else {
      body.className = 'message-card__content';
      body.textContent = content;
    }
    card.appendChild(body);
    return card;
  }

  // The model a reply came from, beside the role. `text-capitalize` is for
  // roles, not model ids, so this one is a plain badge.
  function modelBadge(model) {
    var el = document.createElement('span');
    el.className = 'badge';
    el.textContent = model;
    return el;
  }

  // The "nothing here yet" line in the messages pane; the first message
  // appended replaces it.
  function emptyLine(text) {
    var p = document.createElement('p');
    p.className = 'chat-empty-state text-muted';
    p.textContent = text;
    return p;
  }

  function appendMessageCard(role, content, opts) {
    var area = document.getElementById('messages-area');
    if (!area) return null;
    var placeholder = area.querySelector('.chat-empty-state');
    if (placeholder) placeholder.remove();

    var date = new Date().toISOString().slice(0, 10);
    var card = messageCard(role, content, date, opts);
    area.appendChild(card);
    scrollChatToBottom();
    return card;
  }

  // The content element of a card `messageCard` built.
  function cardBody(card) {
    return card ? card.querySelector('.message-card__content') : null;
  }

  // The assistant card while the model is still thinking.
  function showThinking(contentDiv) {
    if (!contentDiv) return;
    contentDiv.textContent = '';
    var thinking = document.createElement('span');
    thinking.className = 'text-muted chat-thinking';
    thinking.textContent = 'Thinking...';
    contentDiv.appendChild(thinking);
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
    if (container) container.classList.toggle('hidden', !show);
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
        var contentDiv = cardBody(card);
        showThinking(contentDiv);
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
          var cd = cardBody(streamCard);
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
    showThinking(cardBody(card));
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
        var contentDiv = cardBody(streamCard);
        if (contentDiv) contentDiv.innerHTML = renderMarkdown(data.content || 'No response');
        if (data.model) {
          var header = streamCard.querySelector('.message-card__head');
          if (header) header.appendChild(modelBadge(data.model));
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
          var placeholder = list.querySelector('.thread-pane__empty');
          if (placeholder) placeholder.remove();
          // Built with the DOM API and the same classes the server
          // renders (`thread_list_items` in blocks/llm/pages.rs), so the id
          // is never concatenated into markup and the two spellings of a
          // thread card cannot drift again. It used to be an HTML string
          // with inline styles and three `on*` attributes, one of them
          // duplicating the delegated `[data-thread-id]` handler below.
          var card = document.createElement('a');
          card.className = 'card thread-card';
          card.href = threadUrl(id);
          card.dataset.threadId = id;
          card.dataset.active = 'false';
          var row = document.createElement('div');
          row.className = 'thread-card__row';
          var name = document.createElement('span');
          name.className = 'thread-card__title';
          name.textContent = 'New Chat';
          var when = document.createElement('span');
          when.className = 'text-muted thread-card__date';
          when.textContent = new Date().toISOString().slice(0, 10);
          row.appendChild(name);
          row.appendChild(when);
          card.appendChild(row);
          list.insertBefore(card, list.firstChild);
        }
        // On a phone showing the thread list, the conversation pane is not
        // on screen to open the thread in: go to its page, which is a real
        // history entry Back returns from.
        if (conversationShown()) selectThread(id);
        else window.location.assign(threadUrl(id));
      }
    })
    .catch(function (err) {
      console.error('[impresspress] Error creating thread:', err);
    });
  }

  function threadUrl(id) {
    return '/b/llm/threads/' + encodeURIComponent(id);
  }

  // Whether the conversation pane is on screen beside the thread list. On a
  // wide screen it always is; on a phone (`templates::ChatFocus`) the list
  // and the conversation are shown one at a time, and a thread is opened by
  // going to its page rather than swapped in place.
  function conversationShown() {
    var main = document.querySelector('.page--chat > .chat-main');
    return !!main && window.getComputedStyle(main).display !== 'none';
  }

  function selectThread(id) {
    document.getElementById('active-thread-id').value = id;
    var page = document.querySelector('.page--chat');
    if (page) page.setAttribute('data-chat-focus', 'conversation');

    var form = document.getElementById('chat-form');
    if (form) form.classList.remove('chat-form--disabled');
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

        area.textContent = '';
        if (records.length === 0) {
          area.appendChild(emptyLine('No messages yet.'));
        } else {
          records.forEach(function (m) {
            var d = m.data || m;
            var date = (d.created_at || '').slice(0, 10);
            area.appendChild(messageCard(d.role || 'user', d.content || '', date));
          });
        }
        scrollChatToBottom();
      })
      .catch(function (err) {
        console.error('[impresspress] Error loading messages:', err);
      });

    // Toggle the same attributes the server renders: `data-active`, whose
    // highlight colors live in one CSS rule (`.chat-threads
    // .card[data-active="true"]`), so SSR and client-side switching can't
    // drift, and the `aria-current` that names the open thread to a screen
    // reader.
    document.querySelectorAll('[data-thread-id]').forEach(function (el) {
      var active = el.dataset.threadId === id;
      el.dataset.active = active ? 'true' : 'false';
      if (active) el.setAttribute('aria-current', 'page');
      else el.removeAttribute('aria-current');
    });

    history.replaceState({}, '', threadUrl(id));
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

    area.textContent = '';
    messages.forEach(function (m) {
      area.appendChild(messageCard(m.role, m.content, (m.created_at || '').slice(0, 10)));
    });
    scrollChatToBottom();
  }

  // -------------------------------------------------------------------------
  // Public init entry point
  // -------------------------------------------------------------------------

  // The page's own controls, delegated. `#chat-form` is bound by id because
  // there is exactly one composer per page; the three buttons and the model
  // picker declare verbs, because a `data-action` reads back as inert text
  // where an `onclick` value is JavaScript source.
  function bindPageControls() {
    document.addEventListener('submit', function (e) {
      if (e.target && e.target.id === 'chat-form') handleChatSubmit(e);
    });
    document.addEventListener('click', function (e) {
      if (!(e.target instanceof Element)) return;
      var el = e.target.closest('[data-action]');
      if (!el) return;
      var action = el.getAttribute('data-action');
      if (action === 'llm-new-thread') createNewThread();
      else if (action === 'llm-unload-model') unloadLocalModel();
    });
    document.addEventListener('change', function (e) {
      var el = e.target;
      if (!(el instanceof Element)) return;
      if (el.getAttribute('data-action') === 'llm-model-change') onModelChange(el.value);
    });
  }

  function init() {
    bindPageControls();
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
      // On a phone showing the list, the link navigates (see
      // `conversationShown`).
      if (!conversationShown()) return;
      e.preventDefault();
      selectThread(id);
    });
  }

  window.impresspressLlmChat = { init: init };
})();

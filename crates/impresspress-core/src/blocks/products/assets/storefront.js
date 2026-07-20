(function () {
  "use strict";

  const SCRIPT_ORIGIN = (() => {
    try {
      return document.currentScript && document.currentScript.src
        ? new URL(document.currentScript.src, document.baseURI).origin
        : window.location.origin;
    } catch (_) {
      return window.location.origin;
    }
  })();
  const STRIPE_JS_URL = "https://js.stripe.com/clover/stripe.js";
  const ZERO_DECIMAL = new Set([
    "BIF", "CLP", "DJF", "GNF", "JPY", "KMF", "KRW", "MGA", "PYG",
    "RWF", "UGX", "VND", "VUV", "XAF", "XOF", "XPF",
  ]);
  const THREE_DECIMAL = new Set(["BHD", "JOD", "KWD", "OMR", "TND"]);
  let stripeScriptPromise;

  function stripeJs() {
    if (window.Stripe) return Promise.resolve(window.Stripe);
    if (stripeScriptPromise) return stripeScriptPromise;
    stripeScriptPromise = new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = STRIPE_JS_URL;
      script.async = true;
      script.onload = () => window.Stripe
        ? resolve(window.Stripe)
        : reject(new Error("Stripe.js did not initialize"));
      script.onerror = () => reject(new Error("Stripe.js could not be loaded"));
      document.head.appendChild(script);
    });
    return stripeScriptPromise;
  }

  function exponent(currency) {
    const code = String(currency || "").toUpperCase();
    if (ZERO_DECIMAL.has(code)) return 0;
    if (THREE_DECIMAL.has(code)) return 3;
    return 2;
  }

  function money(minor, currency) {
    const code = String(currency || "USD").toUpperCase();
    let value;
    try {
      value = BigInt(String(minor));
    } catch (_) {
      return `${code} —`;
    }
    const places = exponent(code);
    const negative = value < 0n;
    const absolute = negative ? -value : value;
    const divisor = 10n ** BigInt(places);
    const whole = absolute / divisor;
    const fraction = absolute % divisor;
    const amount = places === 0
      ? whole.toString()
      : `${whole}.${fraction.toString().padStart(places, "0")}`;
    return `${negative ? "-" : ""}${code} ${amount}`;
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  class ImpresspressProduct extends HTMLElement {
    static get observedAttributes() {
      return ["api-base", "product-id", "presentation", "payment-link-id"];
    }

    constructor() {
      super();
      this.attachShadow({ mode: "open" });
      this.product = null;
      this.offer = null;
      this.quote = null;
      this.previewTimer = null;
      this.previewRequest = null;
      this.embeddedCheckout = null;
      this.renderShell();
    }

    connectedCallback() {
      this.load();
    }

    attributeChangedCallback(name, oldValue, newValue) {
      if (this.isConnected && oldValue !== newValue) this.load();
    }

    get apiBase() {
      return (this.getAttribute("api-base") || SCRIPT_ORIGIN).replace(/\/$/, "");
    }

    get productId() {
      return this.getAttribute("product-id") || "";
    }

    get presentation() {
      const value = (this.getAttribute("presentation") || "hosted").toLowerCase();
      return ["hosted", "embedded", "payment_link"].includes(value) ? value : "hosted";
    }

    get credentials() {
      const value = this.getAttribute("credentials") || "same-origin";
      return ["omit", "same-origin", "include"].includes(value) ? value : "same-origin";
    }

    endpoint(path) {
      return `${this.apiBase}${path}`;
    }

    async request(path, options) {
      const response = await fetch(this.endpoint(path), {
        credentials: this.credentials,
        ...options,
        headers: {
          Accept: "application/json",
          ...(options && options.body ? { "Content-Type": "application/json" } : {}),
          ...((options && options.headers) || {}),
        },
      });
      const type = response.headers.get("content-type") || "";
      const body = type.includes("json") ? await response.json() : null;
      if (!response.ok) {
        const message = body && (body.message || body.error)
          ? String(body.message || body.error)
          : `Request failed (${response.status})`;
        throw new Error(message);
      }
      return body;
    }

    renderShell() {
      this.shadowRoot.innerHTML = `
        <style>
          :host { --ip-accent:#2563eb; --ip-border:#dbe3ee; --ip-bg:#fff; --ip-text:#172033; --ip-muted:#617089; display:block; color:var(--ip-text); font:14px/1.5 ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
          * { box-sizing:border-box; }
          .card { overflow:hidden; border:1px solid var(--ip-border); border-radius:16px; background:var(--ip-bg); box-shadow:0 14px 34px rgba(23,32,51,.08); }
          .media { display:none; width:100%; max-height:360px; object-fit:cover; background:#f2f5f9; }
          .body { padding:22px; }
          h2 { margin:0; font-size:1.5rem; line-height:1.2; }
          .description,.muted { color:var(--ip-muted); }
          .description { margin:.5rem 0 1.25rem; white-space:pre-wrap; }
          .grid { display:grid; grid-template-columns:repeat(auto-fit,minmax(190px,1fr)); gap:14px; }
          .field { display:grid; gap:6px; }
          label { display:grid; gap:6px; font-weight:650; }
          .help { color:var(--ip-muted); font-size:.84rem; font-weight:400; }
          input,select,button { width:100%; min-height:42px; border-radius:9px; border:1px solid var(--ip-border); background:#fff; color:inherit; padding:8px 10px; font:inherit; }
          input[type=checkbox] { width:20px; min-height:20px; margin:0; accent-color:var(--ip-accent); }
          .check { display:flex; align-items:center; gap:9px; padding-top:7px; }
          .check label { display:inline; }
          button { border-color:var(--ip-accent); background:var(--ip-accent); color:#fff; cursor:pointer; font-weight:750; }
          button:disabled { cursor:not-allowed; opacity:.6; }
          .quote { margin:18px 0; border-top:1px solid var(--ip-border); border-bottom:1px solid var(--ip-border); padding:14px 0; }
          .line,.total { display:flex; justify-content:space-between; gap:16px; padding:4px 0; }
          .line[hidden] { display:none; }
          .total { margin-top:7px; border-top:1px dashed var(--ip-border); padding-top:10px; font-size:1.08rem; font-weight:800; }
          .status { min-height:24px; margin:11px 0 0; color:var(--ip-muted); }
          .status.error { color:#b42318; }
          .status.success { color:#087443; }
          .embedded { margin-top:16px; }
          [hidden] { display:none !important; }
          @media (max-width:560px) { .body { padding:17px; } .grid { grid-template-columns:1fr; } }
        </style>
        <section class="card" aria-busy="true">
          <img class="media" alt="">
          <div class="body">
            <h2 class="title">Loading product…</h2>
            <p class="description" hidden></p>
            <form novalidate>
              <div class="grid">
                <label class="offer-wrap">Offer<select class="offer"></select></label>
                <label class="quantity-wrap">Quantity<input class="quantity" type="number" inputmode="numeric" min="1" step="1" value="1" required></label>
                <label class="email-wrap">Email<input class="email" type="email" autocomplete="email" maxlength="254"></label>
              </div>
              <div class="grid variables"></div>
              <div class="quote" hidden>
                <div class="lines"></div>
                <div class="line discount" hidden><span>Discount</span><span></span></div>
                <div class="line tax" hidden><span>Tax</span><span></span></div>
                <div class="total"><span>Total</span><span></span></div>
              </div>
              <button class="checkout" type="submit" disabled>Continue to checkout</button>
              <p class="status" role="status" aria-live="polite"></p>
            </form>
            <div class="embedded" hidden></div>
          </div>
        </section>`;
      this.card = this.shadowRoot.querySelector(".card");
      this.form = this.shadowRoot.querySelector("form");
      this.titleNode = this.shadowRoot.querySelector(".title");
      this.descriptionNode = this.shadowRoot.querySelector(".description");
      this.imageNode = this.shadowRoot.querySelector(".media");
      this.offerNode = this.shadowRoot.querySelector(".offer");
      this.quantityNode = this.shadowRoot.querySelector(".quantity");
      this.quantityWrap = this.shadowRoot.querySelector(".quantity-wrap");
      this.variablesNode = this.shadowRoot.querySelector(".variables");
      this.emailWrap = this.shadowRoot.querySelector(".email-wrap");
      this.emailNode = this.shadowRoot.querySelector(".email");
      this.quoteNode = this.shadowRoot.querySelector(".quote");
      this.linesNode = this.shadowRoot.querySelector(".lines");
      this.discountNode = this.shadowRoot.querySelector(".discount");
      this.taxNode = this.shadowRoot.querySelector(".tax");
      this.totalNode = this.shadowRoot.querySelector(".total span:last-child");
      this.checkoutNode = this.shadowRoot.querySelector(".checkout");
      this.statusNode = this.shadowRoot.querySelector(".status");
      this.embeddedNode = this.shadowRoot.querySelector(".embedded");
      this.offerNode.addEventListener("change", () => this.selectOffer());
      this.form.addEventListener("input", () => this.schedulePreview());
      this.form.addEventListener("change", () => this.schedulePreview());
      this.form.addEventListener("submit", (event) => this.checkout(event));
    }

    async load() {
      this.card.setAttribute("aria-busy", "true");
      this.setStatus("Loading product…");
      this.checkoutNode.disabled = true;
      if (!this.productId) {
        this.fail(new Error("The product-id attribute is required"));
        return;
      }
      try {
        this.product = await this.request(`/b/products/storefront/${encodeURIComponent(this.productId)}`);
        this.renderProduct();
        await this.resumeReceipt();
        this.dispatchEvent(new CustomEvent("impresspress:ready", { detail: this.product }));
      } catch (error) {
        this.fail(error);
      } finally {
        this.card.setAttribute("aria-busy", "false");
      }
    }

    renderProduct() {
      this.titleNode.textContent = this.product.name || "Product";
      this.descriptionNode.textContent = this.product.description || "";
      this.descriptionNode.hidden = !this.product.description;
      const imageUrl = this.product.image_url || "";
      this.imageNode.hidden = !imageUrl;
      this.imageNode.style.display = imageUrl ? "block" : "none";
      if (imageUrl) {
        this.imageNode.src = imageUrl;
        this.imageNode.alt = this.product.name || "Product";
      } else {
        this.imageNode.removeAttribute("src");
      }
      this.offerNode.replaceChildren();
      (this.product.offers || []).forEach((offer) => {
        const option = element("option", "", offer.name || "Offer");
        option.value = offer.id;
        this.offerNode.appendChild(option);
      });
      this.shadowRoot.querySelector(".offer-wrap").hidden = (this.product.offers || []).length < 2;
      if (!(this.product.offers || []).length) throw new Error("This product has no active offers");
      this.selectOffer();
    }

    selectOffer() {
      this.offer = (this.product.offers || []).find((item) => item.id === this.offerNode.value)
        || this.product.offers[0];
      this.offerNode.value = this.offer.id;
      this.quote = null;
      this.quoteNode.hidden = true;
      this.checkoutNode.disabled = true;
      this.variablesNode.replaceChildren();
      [...(this.offer.variables || [])]
        .sort((a, b) => (a.sort_order || 0) - (b.sort_order || 0))
        .forEach((variable) => this.renderVariable(variable));
      this.emailWrap.hidden = this.presentation === "payment_link";
      this.quantityWrap.hidden = this.presentation === "payment_link";
      this.variablesNode.hidden = this.presentation === "payment_link";
      this.checkoutNode.textContent = this.presentation === "payment_link"
        ? "Buy with Stripe"
        : this.presentation === "embedded"
          ? "Open secure checkout"
          : "Continue to secure checkout";
      if (this.presentation === "payment_link") {
        const link = this.paymentLink();
        this.quote = link && link.pricing;
        if (this.quote) this.renderQuote();
        this.checkoutNode.disabled = !link;
        this.setStatus(link ? "" : "No reusable Payment Link is available for this offer.");
      } else {
        this.schedulePreview(0);
      }
    }

    renderVariable(variable) {
      const id = `ip-${this.productId}-${variable.key}`;
      const wrap = element("div", variable.kind === "boolean" ? "check" : "field");
      let input;
      if (variable.kind === "select" || variable.kind === "multi_select") {
        input = document.createElement("select");
        input.multiple = variable.kind === "multi_select";
        if (!variable.required && !input.multiple) input.appendChild(element("option", "", "Choose…"));
        (variable.allowed_values || []).forEach((value) => {
          const option = element("option", "", value);
          option.value = value;
          const defaults = Array.isArray(variable.default_value)
            ? variable.default_value
            : [variable.default_value];
          option.selected = defaults.includes(value);
          input.appendChild(option);
        });
      } else {
        input = document.createElement("input");
        if (variable.kind === "boolean") {
          input.type = "checkbox";
          input.checked = variable.default_value === true;
        } else if (variable.kind === "number" || variable.kind === "integer") {
          input.type = "number";
          input.inputMode = variable.kind === "integer" ? "numeric" : "decimal";
          input.step = variable.kind === "integer" ? "1" : (variable.step || "any");
          if (variable.minimum != null) input.min = String(variable.minimum);
          if (variable.maximum != null) input.max = String(variable.maximum);
          if (variable.default_value != null) input.value = String(variable.default_value);
        } else if (variable.kind === "date" || variable.kind === "date_time") {
          input.type = variable.kind === "date" ? "date" : "datetime-local";
          if (variable.minimum != null) input.min = String(variable.minimum);
          if (variable.maximum != null) input.max = String(variable.maximum);
          if (variable.default_value != null) input.value = String(variable.default_value);
        } else {
          input.type = "text";
          if (variable.maximum_length) input.maxLength = variable.maximum_length;
          if (variable.default_value != null) input.value = String(variable.default_value);
        }
      }
      input.id = id;
      input.dataset.variable = variable.key;
      input.dataset.kind = variable.kind;
      input.required = !!variable.required;
      const label = element("label", "", variable.label || variable.key);
      label.htmlFor = id;
      if (variable.kind === "boolean") {
        wrap.append(input, label);
      } else {
        wrap.append(label, input);
      }
      if (variable.help_text) wrap.appendChild(element("span", "help", variable.help_text));
      this.variablesNode.appendChild(wrap);
    }

    inputs() {
      const values = {};
      this.variablesNode.querySelectorAll("[data-variable]").forEach((input) => {
        const kind = input.dataset.kind;
        if (kind === "boolean") values[input.dataset.variable] = input.checked;
        else if (kind === "multi_select") {
          values[input.dataset.variable] = Array.from(input.selectedOptions, (option) => option.value);
        } else if (kind === "number" || kind === "integer") {
          if (input.value !== "") values[input.dataset.variable] = Number(input.value);
        } else if (input.value !== "" || input.required) {
          values[input.dataset.variable] = input.value;
        }
      });
      return values;
    }

    schedulePreview(delay) {
      clearTimeout(this.previewTimer);
      this.previewTimer = setTimeout(() => this.preview(), delay === undefined ? 220 : delay);
    }

    async preview() {
      if (!this.offer || !this.form.reportValidity()) return;
      if (this.previewRequest) this.previewRequest.abort();
      this.previewRequest = new AbortController();
      this.setStatus("Calculating…");
      try {
        const quantity = Number(this.quantityNode.value);
        const quote = await this.request("/b/products/pricing/preview", {
          method: "POST",
          signal: this.previewRequest.signal,
          body: JSON.stringify({ offer_id: this.offer.id, quantity, inputs: this.inputs() }),
        });
        this.quote = quote;
        this.renderQuote();
        this.checkoutNode.disabled = this.presentation === "payment_link"
          ? !this.paymentLink()
          : false;
        this.setStatus(this.presentation === "payment_link" && !this.paymentLink()
          ? "No reusable Payment Link is available for this offer."
          : "");
        this.dispatchEvent(new CustomEvent("impresspress:quote", { detail: quote }));
      } catch (error) {
        if (error.name !== "AbortError") this.fail(error);
      }
    }

    renderQuote() {
      this.linesNode.replaceChildren();
      (this.quote.components || []).filter((item) => item.included).forEach((item) => {
        const row = element("div", "line");
        row.append(
          element("span", "", `${item.label}${item.quantity > 1 ? ` × ${item.quantity}` : ""}`),
          element("span", "", money(item.total_amount_minor, this.quote.amounts.currency)),
        );
        this.linesNode.appendChild(row);
      });
      const amounts = this.quote.amounts;
      this.discountNode.hidden = !amounts.discount_minor;
      this.discountNode.lastElementChild.textContent = money(-amounts.discount_minor, amounts.currency);
      this.taxNode.hidden = !amounts.tax_minor;
      this.taxNode.lastElementChild.textContent = money(amounts.tax_minor, amounts.currency);
      this.totalNode.textContent = money(amounts.total_minor, amounts.currency)
        + (this.offer.mode === "subscription" ? this.recurrenceLabel() : "");
      this.quoteNode.hidden = false;
    }

    recurrenceLabel() {
      const interval = this.offer.recurring_interval || "period";
      const count = this.offer.interval_count || 1;
      return count === 1 ? ` / ${interval}` : ` / ${count} ${interval}s`;
    }

    paymentLink() {
      const links = this.offer && this.offer.payment_links ? this.offer.payment_links : [];
      const requested = this.getAttribute("payment-link-id");
      return (requested && links.find((link) => link.id === requested)) || links[0] || null;
    }

    returnUrl(kind) {
      const attribute = kind === "success" ? "success-url" : "cancel-url";
      const url = new URL(this.getAttribute(attribute) || window.location.href, window.location.href);
      url.searchParams.set("impresspress_checkout", kind);
      url.searchParams.delete("session_id");
      const plain = url.toString();
      return kind === "success"
        ? `${plain}${plain.includes("?") ? "&" : "?"}session_id={CHECKOUT_SESSION_ID}`
        : plain;
    }

    receiptKey() {
      return `impresspress:receipt:${this.apiBase}:${this.productId}`;
    }

    rememberReceipt(checkout) {
      try {
        sessionStorage.setItem(this.receiptKey(), JSON.stringify({
          order_id: checkout.order_id,
          receipt_token: checkout.receipt_token,
          expires_at: checkout.receipt_token_expires_at,
        }));
      } catch (_) {
        // Checkout can continue; the guest return status simply cannot resume.
      }
    }

    async checkout(event) {
      event.preventDefault();
      if (!this.form.reportValidity() || !this.quote) return;
      const link = this.paymentLink();
      if (this.presentation === "payment_link") {
        if (!link) return this.fail(new Error("No reusable Payment Link is available"));
        window.location.assign(link.url);
        return;
      }

      this.checkoutNode.disabled = true;
      this.setStatus("Preparing secure checkout…");
      try {
        let Stripe;
        let storefrontConfig;
        if (this.presentation === "embedded") {
          [Stripe, storefrontConfig] = await Promise.all([
            stripeJs(),
            this.request("/b/products/storefront/config"),
          ]);
          if (!storefrontConfig.embedded_checkout_available || !storefrontConfig.stripe_publishable_key) {
            throw new Error("Embedded Checkout is not configured");
          }
        }
        const checkout = await this.request("/b/products/checkout", {
          method: "POST",
          body: JSON.stringify({
            offer_id: this.offer.id,
            quantity: Number(this.quantityNode.value),
            inputs: this.inputs(),
            presentation: this.presentation,
            success_url: this.returnUrl("success"),
            cancel_url: this.returnUrl("cancel"),
            buyer_email: this.emailNode.value || undefined,
          }),
        });
        this.rememberReceipt(checkout);
        this.dispatchEvent(new CustomEvent("impresspress:checkout", { detail: checkout }));
        if (this.presentation === "hosted") {
          if (!checkout.checkout_url) throw new Error("Checkout URL was not returned");
          window.location.assign(checkout.checkout_url);
          return;
        }
        if (!checkout.client_secret) throw new Error("Embedded Checkout client secret was not returned");
        if (this.embeddedCheckout) this.embeddedCheckout.destroy();
        const stripe = Stripe(storefrontConfig.stripe_publishable_key);
        this.embeddedCheckout = await stripe.initEmbeddedCheckout({
          fetchClientSecret: async () => checkout.client_secret,
        });
        this.form.hidden = true;
        this.embeddedNode.hidden = false;
        this.embeddedCheckout.mount(this.embeddedNode);
      } catch (error) {
        this.fail(error);
        this.checkoutNode.disabled = false;
      }
    }

    async resumeReceipt() {
      const marker = new URL(window.location.href).searchParams.get("impresspress_checkout");
      if (marker === "cancel") {
        this.setStatus("Checkout was canceled. You can review your choices and try again.");
        return;
      }
      if (marker !== "success") return;
      let receipt;
      try {
        receipt = JSON.parse(sessionStorage.getItem(this.receiptKey()) || "null");
      } catch (_) {
        receipt = null;
      }
      if (!receipt || !receipt.order_id || !receipt.receipt_token) {
        this.setStatus("Payment is returning from Stripe. Sign in to view the order if confirmation does not appear.");
        return;
      }
      this.setStatus("Confirming payment with Stripe…");
      for (let attempt = 0; attempt < 15; attempt += 1) {
        try {
          const status = await this.request(
            `/b/products/orders/${encodeURIComponent(receipt.order_id)}/status?receipt_token=${encodeURIComponent(receipt.receipt_token)}`,
          );
          this.dispatchEvent(new CustomEvent("impresspress:order-status", { detail: status }));
          if (["completed", "refunded", "failed"].includes(status.status)) {
            const successful = status.status === "completed" || status.status === "refunded";
            this.setStatus(
              successful
                ? `Payment confirmed — ${money(status.amounts.total_minor, status.amounts.currency)}.`
                : "Payment could not be confirmed. Please try again or contact support.",
              successful ? "success" : "error",
            );
            if (successful) {
              try { sessionStorage.removeItem(this.receiptKey()); } catch (_) {}
            }
            return;
          }
        } catch (error) {
          if (attempt === 14) return this.fail(error);
        }
        await new Promise((resolve) => setTimeout(resolve, 2000));
      }
      this.setStatus("Payment is still processing. Check your order page shortly.");
    }

    setStatus(message, kind) {
      this.statusNode.textContent = message || "";
      this.statusNode.className = `status${kind ? ` ${kind}` : ""}`;
    }

    fail(error) {
      const message = error && error.message ? error.message : "Something went wrong";
      this.setStatus(message, "error");
      this.dispatchEvent(new CustomEvent("impresspress:error", { detail: { message } }));
    }
  }

  if (!customElements.get("impresspress-product")) {
    customElements.define("impresspress-product", ImpresspressProduct);
  }
})();

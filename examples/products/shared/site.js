(async function () {
  "use strict";

  const response = await fetch("./commerce.fixture.json", { credentials: "same-origin" });
  if (!response.ok) throw new Error(`Example fixture failed to load (${response.status})`);
  const fixture = await response.json();
  window.__commerceExample = fixture;

  const text = (selector, value) => {
    document.querySelectorAll(selector).forEach((node) => { node.textContent = String(value || ""); });
  };
  const list = (selector, values) => {
    document.querySelectorAll(selector).forEach((node) => {
      node.replaceChildren(...(values || []).map((value) => {
        const item = document.createElement("li");
        item.textContent = value;
        return item;
      }));
    });
  };
  const badges = (selector, values) => {
    document.querySelectorAll(selector).forEach((node) => {
      node.replaceChildren(...(values || []).map((value) => {
        const item = document.createElement("span");
        item.className = "badge";
        item.textContent = value;
        return item;
      }));
    });
  };

  text("[data-template]", fixture.developer.template);
  text("[data-ownership]", fixture.developer.ownership);
  text("[data-fulfillment]", fixture.developer.fulfillment);
  list("[data-developer-journey]", fixture.developer.journey);
  list("[data-operator-journey]", fixture.operator.journey);
  badges("[data-stripe-features]", fixture.developer.stripe_features);

  const widget = document.querySelector("impresspress-product");
  document.querySelectorAll("[data-presentation]").forEach((button) => {
    button.addEventListener("click", () => {
      widget.setAttribute("presentation", button.dataset.presentation);
      document.querySelectorAll("[data-presentation]").forEach((item) => {
        item.setAttribute("aria-pressed", String(item === button));
      });
    });
  });

  document.dispatchEvent(new CustomEvent("commerce-example:ready", { detail: fixture }));
}()).catch((error) => {
  const output = document.querySelector("[data-example-error]");
  if (output) output.textContent = error.message;
});

// Plain Web Component, no imports — so the bake stays offline; compiled & minified into app.js (build.rs).
class ClickCounter extends HTMLElement {
  // This inline comment is dropped by minification; the one-line banner above is kept
  // (oxc preserves a leading banner comment, the way a license header would survive).
  private count = 0; // initialised, so the Lit-preset class-field transform keeps it

  connectedCallback(): void {
    const button = document.createElement("button");
    button.type = "button";

    const paint = (): void => {
      const noun = this.count === 1 ? "time" : "times";
      button.textContent = `clicked ${this.count} ${noun}`;
    };

    button.addEventListener("click", () => {
      this.count += 1;
      paint();
    });

    paint();
    this.replaceChildren(button);
  }
}

customElements.define("click-counter", ClickCounter);

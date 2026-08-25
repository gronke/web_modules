/*!
 * embedded example: a plain Web Component, no imports, so the bake stays offline.
 * @license MIT
 */
// Compiled into app.js by build.rs under `Comments::Collect`: a normal comment like this
// one is stripped by minification, while the legal banner above moves into the
// app.js.LEGAL.txt sidecar, a pointer comment left in its place.
class ClickCounter extends HTMLElement {
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

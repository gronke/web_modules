// A dependency-free Web Component (no bare imports → nothing to vendor, fully offline).
// web-modules compiles this .ts to .js on the fly in dev, and bakes it for the release bundle.
class LiveClock extends HTMLElement {
  private timer = 0; // initialised, so the Lit-preset class-field transform keeps it

  connectedCallback(): void {
    const tick = (): void => {
      this.textContent = new Date().toLocaleTimeString();
    };
    tick();
    this.timer = window.setInterval(tick, 1000);
  }

  disconnectedCallback(): void {
    window.clearInterval(this.timer);
  }
}

customElements.define("live-clock", LiveClock);

import { LitElement, html } from 'lit';

// A tiny, self-contained Lit component — decorator-free (`static properties`), so only `lit` is
// vendored and no transform runtime is needed. Authored in TypeScript and compiled to browser JS by
// `web-modules build` (oxc) — no tsc, no bundler. The bare `lit` specifier resolves through the
// generated import map. Rendered into the light DOM so the page's compiled styles.css applies.
export class HelloCard extends LitElement {
  static properties = { count: { type: Number } };

  count = 0;

  createRenderRoot(): this {
    return this;
  }

  render() {
    return html`
      <div class="card">
        <h1>web_modules</h1>
        <p class="lede">
          This page was built and deployed to GitHub Pages with <strong>no Node</strong>: the
          <code>lit</code> dependency was vendored from npm, the TypeScript and SCSS were transformed
          by oxc and grass, and it is served as native ES modules via an import map.
        </p>
        <button @click=${() => (this.count += 1)}>Clicked ${this.count}×</button>
      </div>
    `;
  }
}

customElements.define('hello-card', HelloCard);

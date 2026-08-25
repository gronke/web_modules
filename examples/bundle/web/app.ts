/*!
 * web_modules bundle demo
 * @license MIT
 */
import { LitElement, html } from 'lit';

// The gh-pages component's sibling, with one twist: the button pulls `./lazy.js` in via a
// dynamic import(). Under `bundle`, lit and its sub-packages are inlined into app.js while
// the dynamically-imported module lands in content-hashed chunks/: the split point
// survives bundling. The specifier names the OUTPUT file (lazy.js, not lazy.ts): sources
// import what the browser will load, bundled or not.
export class BundleCard extends LitElement {
  static properties = { fired: { type: Number } };

  fired = 0;

  createRenderRoot(): this {
    return this;
  }

  render() {
    return html`
      <div class="card">
        <h1>bundled, not buildless</h1>
        <p class="lede">
          One entry, one URL: <code>app.js</code> ships with <code>lit</code> inlined, no
          import map in sight, and the celebration below arrives on demand from a
          content-hashed chunk.
        </p>
        <button @click=${this.celebrate}>
          Celebrate${this.fired > 0 ? html` ×${this.fired}` : ''}
        </button>
      </div>
    `;
  }

  private async celebrate(): Promise<void> {
    const { celebrate } = await import('./lazy.js');
    celebrate();
    this.fired += 1;
  }
}

customElements.define('bundle-card', BundleCard);

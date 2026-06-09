import { LitElement, html } from 'lit';

// A reusable Lit counter that depends only on `lit`, so it composes cleanly into other
// apps (see ../../compose, which charts its clicks over time). On each click it
// increments and emits a `counter-tick` CustomEvent carrying the click's epoch (ms).
//
// It renders Bootstrap-*classed* markup into the light DOM, but uses no Bootstrap *JS* —
// activating the tooltip is left to the host (the standalone example does it in app.ts),
// so the component itself stays dependency-light and reusable.
export class CounterCard extends LitElement {
  static properties = {
    count: { type: Number },
  };

  count = 0;

  // Render into the light DOM so Bootstrap's global stylesheet applies.
  createRenderRoot(): this {
    return this;
  }

  private increment(): void {
    this.count += 1;
    this.dispatchEvent(
      new CustomEvent<number>('counter-tick', {
        detail: Date.now(),
        bubbles: true,
        composed: true,
      }),
    );
  }

  render() {
    return html`
      <div class="card shadow-sm mx-auto" style="max-width: 22rem">
        <div class="card-body text-center">
          <h5 class="card-title">web-modules · Lit counter</h5>
          <p class="display-4 my-3">${this.count}</p>
          <button
            class="btn btn-primary"
            data-bs-toggle="tooltip"
            data-bs-placement="bottom"
            title="Tooltip via Bootstrap JS + Popper — vendored, no bundler"
            @click=${() => this.increment()}
          >
            Increment
          </button>
        </div>
      </div>
    `;
  }
}

customElements.define('counter-card', CounterCard);

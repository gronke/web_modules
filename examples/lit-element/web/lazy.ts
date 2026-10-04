import { html, render } from 'lit';

// Loaded on the first click; class-map loads through the import map's `lit/` key.
export async function note(card: Element): Promise<void> {
  const { classMap } = await import(`lit/directives/class-map.js`);
  const host = document.createElement('div');
  card.after(host);
  render(
    html`<p data-lazy class=${classMap({ 'text-center': true, 'text-secondary': true, 'mt-3': true })}>
      Loaded on demand with import()
    </p>`,
    host,
  );
}

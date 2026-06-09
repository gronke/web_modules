import './counter.js';
import { Tooltip } from 'bootstrap';

// Standalone entry point. The reusable counter (counter.ts) carries the tooltip *markup*
// but no Bootstrap JS; the host wires the behavior. Here we activate Bootstrap tooltips
// once the component has rendered — demonstrating vendored Bootstrap JS + Popper with no
// bundler. (The compose example reuses counter.ts without any of this.)
//
// Authored in TypeScript; web-modules transforms it to browser JS with oxc on the fly —
// no tsc, no bundler. The bare `bootstrap`/`lit` specifiers resolve through the import map.
const card = document.querySelector('counter-card') as
  | (Element & { updateComplete?: Promise<unknown> })
  | null;
await card?.updateComplete;
card?.querySelectorAll('[data-bs-toggle="tooltip"]').forEach((el) => new Tooltip(el));

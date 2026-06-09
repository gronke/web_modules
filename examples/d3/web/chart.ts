// `d3` is loaded as a UMD global (see index.html) — no import-map entry needed.
// web-modules transforms this TypeScript to JS on the fly; the types are erased.
//
// The render is exported as a function so other apps can chart their own data with it
// (see ../../compose, which feeds it a live click timeline). The standalone demo at the
// bottom only runs when this example's `#chart` element is present, so importing the
// module elsewhere doesn't draw the demo.
declare const d3: any;

export function renderChart(selector: string, values: number[]): void {
  const width = 520;
  const height = 240;
  const gap = 6;
  const band = width / Math.max(1, values.length);
  const max = Math.max(1, ...values);
  const scale = (value: number): number => (value / max) * (height - 20);

  d3.select(selector)
    .selectAll('rect')
    .data(values)
    .join('rect')
    .attr('x', (_d: number, i: number) => i * band + gap)
    .attr('y', (d: number) => height - scale(d))
    .attr('width', Math.max(0, band - gap * 2))
    .attr('height', (d: number) => scale(d))
    .attr('rx', 4)
    .attr('fill', '#0d6efd');
}

if (typeof document !== 'undefined' && document.querySelector('#chart')) {
  renderChart('#chart', [4, 8, 15, 16, 23, 42]);
}

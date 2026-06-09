// Glue for the composed app. Both imports use bare specifiers resolved through the
// co-generated import map: `counter/` → the lit-element example's source, `chart/` → the
// d3 example's source (see ../src/main.rs). Importing a component *by name* across mounts
// is the whole point — neither sibling knows about this app.
import 'counter/counter.js'; // registers <counter-card>, which emits `counter-tick`
import { renderChart } from 'chart/chart.js';

// Each click's epoch (ms). The counter emits them; we chart their distribution.
const presses: number[] = [];

document.addEventListener('counter-tick', (event) => {
  presses.push((event as CustomEvent<number>).detail);
  renderChart('#presses', distribution(presses));
});

// Bin the presses into one-second buckets since the first click → count per bucket.
// That's the "press distribution over time" the chart draws, growing as you click.
function distribution(epochs: number[]): number[] {
  if (epochs.length === 0) return [];
  const bucketMs = 1000;
  const start = epochs[0];
  const last = epochs[epochs.length - 1];
  const span = Math.floor((last - start) / bucketMs) + 1;
  const buckets = new Array<number>(span).fill(0);
  for (const epoch of epochs) {
    buckets[Math.floor((epoch - start) / bucketMs)] += 1;
  }
  return buckets;
}

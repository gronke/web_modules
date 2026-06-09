import { useEffect } from "react";
import { createRoot } from "react-dom/client";
import { create } from "zustand";

// A zustand store. zustand is a *separate* npm dependency that itself imports React
// (it calls `useSyncExternalStore` internally). The component below imports React too.
// For the hooks in both to work, the bundle must contain exactly ONE React instance,
// shared by the app and by zustand — a duplicate would throw "invalid hook call" at
// runtime. rolldown deduplicates React to a single copy; this app running in the browser
// (the Playwright test) is that proof, live. See ../README.md.
interface CounterState {
  count: number;
  increment: () => void;
}

const useCounter = create<CounterState>()((set) => ({
  count: 0,
  increment: () => set((state) => ({ count: state.count + 1 })),
}));

function App() {
  // State from zustand (its hook runs React's useSyncExternalStore)...
  const count = useCounter((state) => state.count);
  const increment = useCounter((state) => state.increment);

  // ...and a plain React effect, so React's own dispatcher is exercised alongside
  // zustand's. If React were duplicated, one of these two would be wired to the wrong
  // dispatcher and break. The marker lets the e2e assert React actually mounted.
  useEffect(() => {
    document.body.dataset.reactReady = "1";
  }, []);

  return (
    <main>
      <h1>React + zustand, bundled by web-modules</h1>
      <p>
        React (CommonJS) and zustand were installed into <code>node_modules/</code> and
        bundled into one browser ES module — no Node, no CDN, pure Rust.
      </p>
      <button type="button" onClick={increment}>
        count {count}
      </button>
    </main>
  );
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(<App />);
}

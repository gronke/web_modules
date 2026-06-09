// React 18 is loaded as UMD globals by the classic <script> tags in index.html
// (window.React / window.ReactDOM) — no import, no bundler. We type those globals from
// @types/react without *importing* them: an `import` would be a bare specifier the browser
// (with no import map and no bundler) couldn't resolve. `typeof import(...)` is a type-only
// query, erased at compile time, so nothing is emitted.
declare const React: typeof import("react");
declare const ReactDOM: typeof import("react-dom/client");

const { createElement: h, useState } = React;

// A counter built with React.createElement instead of JSX — so the source is plain TS that
// needs no JSX transform and no React import, yet still drives a real React render + hook.
function Counter() {
  const [count, setCount] = useState(0);
  return h(
    "button",
    { type: "button", onClick: () => setCount((c) => c + 1) },
    `count ${count}`,
  );
}

const root = document.getElementById("root");
if (root) {
  ReactDOM.createRoot(root).render(h(Counter));
}

export {};

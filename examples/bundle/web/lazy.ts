// Loaded on demand via import("./lazy.js") from app.ts; under `bundle` this module
// becomes a content-hashed chunk in chunks/, fetched from the network on the first click
// only. Deliberately dependency-free, so the chunk's provenance stays obvious.
export function celebrate(): void {
  for (let i = 0; i < 24; i += 1) {
    const piece = document.createElement('span');
    piece.className = 'confetti';
    piece.style.setProperty('--hue', String((i * 137) % 360));
    piece.style.left = `${Math.random() * 100}vw`;
    piece.style.animationDelay = `${Math.random() * 0.4}s`;
    document.body.append(piece);
    setTimeout(() => piece.remove(), 1800);
  }
}

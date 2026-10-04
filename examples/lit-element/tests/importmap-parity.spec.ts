import { readFileSync } from 'node:fs';
import { test, expect } from '@playwright/test';

// The table `tests/importmap_parity.rs` holds `Importmap::resolve` to; null means TypeError.
// `address` is the URL Standard's answer, null where an engine throws against it; an engine's
// own key holds its answer where it departs from `address`.
type Answer = string | null;
type Case = { specifier: string; address: Answer; why: string } & Partial<Record<string, Answer>>;
const table: { imports: Record<string, string>; cases: Case[] } = JSON.parse(
  readFileSync(new URL('../../../tests/importmap_parity.json', import.meta.url), 'utf8'),
);

const html = `<!doctype html>
<meta charset="utf-8">
<script type="importmap">${JSON.stringify({ imports: table.imports }).replace(/</g, '\\u003c')}</script>
<script type="module">
window.resolveAll = (specifiers) => specifiers.map((s) => {
  try { return import.meta.resolve(s); } catch (e) { return e instanceof TypeError ? null : String(e); }
});
</script>`;

// At the root of an http and an https origin: `./` addresses resolve against `/`, as `resolve`
// reads them, and each scheme drops its own default port.
const pages = ['/importmap-parity.html', 'https://importmap-parity.test/importmap-parity.html'];

// The URL `address` names on `page`, joined by hand so a non-canonical address cannot match.
function literally(address: string, page: URL): string {
  if (address.startsWith('//')) return page.protocol + address;
  if (address.startsWith('/')) return page.origin + address;
  return address;
}

test('import.meta.resolve answers every case of the import-map table', async ({
  page,
  browserName,
}) => {
  await page.route('**/importmap-parity.html', (route) =>
    route.fulfill({ contentType: 'text/html; charset=utf-8', body: html }),
  );
  const answers: { url: URL; resolved: Answer[] }[] = [];
  for (const path of pages) {
    await page.goto(path);
    await page.waitForFunction(() => 'resolveAll' in window);
    const resolved: Answer[] = await page.evaluate(
      (specifiers) =>
        (window as unknown as { resolveAll: (s: string[]) => Answer[] }).resolveAll(specifiers),
      table.cases.map((c) => c.specifier),
    );
    answers.push({ url: new URL(page.url()), resolved });
  }

  const mismatches = table.cases.flatMap((c, i) => {
    const address = browserName in c ? (c[browserName] as Answer) : c.address;
    const got = answers.map(({ resolved }) => resolved[i]);
    const want = answers.map(({ url }) => (address === null ? null : new URL(address, url).href));
    // Written as one page at least writes it, since the other may drop the port.
    const written =
      address === null || answers.some(({ url, resolved }) => resolved[i] === literally(address, url));
    return written && got.every((answer, p) => answer === want[p]) ? [] : [{ ...c, want, got }];
  });
  expect(mismatches).toEqual([]);
});

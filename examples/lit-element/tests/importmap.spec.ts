import { test, expect } from '@playwright/test';

// Written by build.rs with `imports::read_module` and `Importmap::resolve`; null means TypeError.
type Resolution = {
  specifiers: Record<string, string | null>;
  static: string[];
  lazy: string[];
};

test('the shipped imports load where resolve predicts', async ({ page, request, baseURL }) => {
  const resolution: Resolution = await (await request.get('/e2e/resolution.json')).json();

  // The polyfill loader is a classic script, outside the module graph, and a debug server
  // reused locally adds its live-reload client.
  const loaded = new Set<string>();
  page.on('request', (req) => {
    const { pathname } = new URL(req.url());
    const outside = ['/web_modules/@webcomponents/', '/_web_modules/'];
    if (req.resourceType() === 'script' && !outside.some((p) => pathname.startsWith(p))) {
      loaded.add(pathname);
    }
  });
  await page.goto('/');
  await expect(page.locator('counter-card .display-4')).toHaveText('3');
  expect([...loaded].sort()).toEqual(resolution.static);

  await page.addScriptTag({
    type: 'module',
    content: `window.resolveAll = (specifiers) => specifiers.map((s) => {
      try { return import.meta.resolve(s); } catch (e) { return e instanceof TypeError ? null : String(e); }
    });`,
  });
  await page.waitForFunction(() => 'resolveAll' in window);
  const specifiers = Object.keys(resolution.specifiers);
  const resolved: (string | null)[] = await page.evaluate(
    (s) => (window as unknown as { resolveAll: (s: string[]) => (string | null)[] }).resolveAll(s),
    specifiers,
  );
  // Joined by hand, so a non-canonical address cannot match.
  const origin = new URL(baseURL!).origin;
  const mismatches = specifiers.flatMap((specifier, i) => {
    const address = resolution.specifiers[specifier];
    const want = address === null ? null : origin + address;
    return resolved[i] === want ? [] : [{ specifier, want, got: resolved[i] }];
  });
  expect(mismatches).toEqual([]);

  const unserved: string[] = [];
  for (const address of new Set(Object.values(resolution.specifiers))) {
    if (address !== null && (await request.get(address)).status() !== 200) unserved.push(address);
  }
  expect(unserved).toEqual([]);

  await page.getByRole('button', { name: 'Increment' }).click();
  await expect(page.locator('[data-lazy]')).toBeVisible();
  expect([...loaded].sort()).toEqual([...resolution.static, ...resolution.lazy].sort());
});

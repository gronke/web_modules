import { test, expect, type Page } from '@playwright/test';

// Collect console errors and failed/4xx-5xx responses. A missing UMD asset would show as a
// failed request; a broken global would throw and surface as a page/console error.
function watchForErrors(page: Page): { consoleErrors: string[]; failed: string[] } {
  const consoleErrors: string[] = [];
  const failed: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('pageerror', (err) => consoleErrors.push(err.message));
  page.on('requestfailed', (req) => failed.push(`${req.failure()?.errorText ?? 'failed'} ${req.url()}`));
  page.on('response', (res) => {
    if (res.status() >= 400) failed.push(`${res.status()} ${res.url()}`);
  });
  return { consoleErrors, failed };
}

test.describe('Classic React (UMD), served by web-modules', () => {
  test('loads React from single-file UMD <script> globals and the counter works', async ({ page }) => {
    const { consoleErrors, failed } = watchForErrors(page);

    await page.goto('/');

    // The UMD globals are present — React was loaded by a plain classic <script>, no bundler,
    // no import map. (The single extracted files are /web_modules/react/react.js and
    // /web_modules/react-dom/react-dom.js.)
    expect(await page.evaluate(() => typeof (window as unknown as { React?: { createElement?: unknown } }).React?.createElement)).toBe('function');
    expect(await page.evaluate(() => typeof (window as unknown as { ReactDOM?: { createRoot?: unknown } }).ReactDOM?.createRoot)).toBe('function');

    // The counter is rendered by React (createRoot) and its state is a useState hook —
    // clicking it proves the global React actually runs, not just loads.
    const button = page.getByRole('button', { name: /count/ });
    await expect(button).toHaveText('count 0');
    await button.click();
    await expect(button).toHaveText('count 1');
    await button.click();
    await expect(button).toHaveText('count 2');

    expect(failed, `failed requests:\n${failed.join('\n')}`).toEqual([]);
    expect(consoleErrors, `console errors:\n${consoleErrors.join('\n')}`).toEqual([]);
  });
});

import { test, expect, type Page } from '@playwright/test';

// Collect console errors and failed/4xx-5xx responses for the duration of a test. A
// duplicated React would log "invalid hook call" here; a bundling problem would show as a
// failed request for the module.
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

test.describe('React + zustand, bundled CJS→ESM by web-modules', () => {
  test('mounts, increments via the zustand store, and runs on a single React instance', async ({ page }) => {
    const { consoleErrors, failed } = watchForErrors(page);

    await page.goto('/');

    // The app's `useEffect` ran ⇒ React mounted and its effect dispatcher is wired up.
    await expect(page.locator('body[data-react-ready="1"]')).toBeAttached();

    // The counter's state lives in a zustand store (a separate dependency that imports
    // React); clicking drives it through React's render. Incrementing proves the store
    // subscription and the component share one working React.
    const button = page.getByRole('button', { name: /count/ });
    await expect(button).toHaveText('count 0');
    await button.click();
    await expect(button).toHaveText('count 1');
    await button.click();
    await expect(button).toHaveText('count 2');

    // The decisive check: two dependencies (the app and zustand) both depend on React, and
    // the bundle must contain exactly ONE React instance — otherwise zustand's
    // `useSyncExternalStore` and the component's `useEffect` hit different dispatchers and
    // React throws "invalid hook call". No errors ⇒ a single, shared React instance.
    expect(failed, `failed requests:\n${failed.join('\n')}`).toEqual([]);
    expect(consoleErrors, `console errors:\n${consoleErrors.join('\n')}`).toEqual([]);
  });
});

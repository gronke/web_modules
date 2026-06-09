import { test, expect, type Page } from '@playwright/test';

// Collect console errors and failed/4xx-5xx responses for the duration of a test —
// a vendored module that fails to resolve through the import map surfaces here.
function watchForErrors(page: Page): { consoleErrors: string[]; failed: string[] } {
  const consoleErrors: string[] = [];
  const failed: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('requestfailed', (req) => failed.push(`${req.failure()?.errorText ?? 'failed'} ${req.url()}`));
  page.on('response', (res) => {
    if (res.status() >= 400) failed.push(`${res.status()} ${res.url()}`);
  });
  return { consoleErrors, failed };
}

test.describe('counter-card · Lit + Bootstrap, vendored by web-modules', () => {
  test('renders, increments, and loads every module from the import map', async ({ page }) => {
    const { consoleErrors, failed } = watchForErrors(page);

    await page.goto('/');

    // The component renders into the light DOM (createRenderRoot returns `this`),
    // so its markup is queryable directly. index.html sets `count="3"`.
    const count = page.locator('counter-card .display-4');
    await expect(count).toHaveText('3');

    await page.getByRole('button', { name: 'Increment' }).click();
    await expect(count).toHaveText('4');

    expect(failed, `failed requests:\n${failed.join('\n')}`).toEqual([]);
    expect(consoleErrors, `console errors:\n${consoleErrors.join('\n')}`).toEqual([]);
  });

  test('Bootstrap tooltip opens below the button so the number stays readable', async ({ page }) => {
    await page.goto('/');
    const button = page.getByRole('button', { name: 'Increment' });
    await button.hover();

    const tooltip = page.locator('.tooltip');
    await expect(tooltip).toBeVisible();

    const b = await button.boundingBox();
    const t = await tooltip.boundingBox();
    expect(b && t).toBeTruthy();
    // `data-bs-placement="bottom"` → the tooltip sits below the button.
    expect(t!.y).toBeGreaterThan(b!.y + b!.height - 1);
  });
});

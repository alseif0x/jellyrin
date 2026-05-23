const { test, expect } = require('@playwright/test');

test('deployed server opens scanned movie detail without frontend request failures', async ({ page, request, baseURL }) => {
  test.skip(process.env.JELLYRIN_E2E_DEPLOYED !== '1', 'Only runs against an already deployed server');
  const adminUser = process.env.JELLYRIN_E2E_ADMIN_USER;
  const adminPassword = process.env.JELLYRIN_E2E_ADMIN_PASSWORD;
  test.skip(!adminUser || !adminPassword, 'Requires JELLYRIN_E2E_ADMIN_USER and JELLYRIN_E2E_ADMIN_PASSWORD');

  const publicInfo = await (await request.get('/System/Info/Public')).json();
  const itemsResponse = await request.get('/Items?IncludeItemTypes=Movie&Limit=1');
  expect(itemsResponse.ok()).toBeTruthy();
  const items = await itemsResponse.json();
  expect(items.TotalRecordCount).toBeGreaterThan(0);
  const movie = items.Items[0];

  const detailResponse = await request.get(`/Items/${movie.Id}`);
  expect(detailResponse.ok()).toBeTruthy();
  const detail = await detailResponse.json();
  expect(detail.Name).toBe(movie.Name);
  expect(detail.Container).toBe('mp4');
  expect(detail.MediaSources).toHaveLength(1);
  expect(detail.People).toEqual([]);
  expect(detail.Studios).toEqual([]);
  expect(detail.GenreItems).toEqual([]);

  const failedResponses = [];
  page.on('response', response => {
    const url = response.url();
    if (response.status() >= 400 && !url.includes('/Branding/Splashscreen')) {
      failedResponses.push(`${response.status()} ${url}`);
    }
  });

  await page.goto(`${baseURL}/web/#/login?serverid=${publicInfo.Id}&url=%2Fhome`);
  await page.getByRole('button', { name: 'Manual Login' }).click();
  await page.locator('#txtManualName').fill(adminUser);
  await page.locator('#txtManualPassword').fill(adminPassword);
  await page.locator('.manualLoginForm .button-submit').click();
  await expect(page).toHaveURL(/\/web\/#\/home/);

  await page.goto(`${baseURL}/web/#/details?id=${movie.Id}`);
  await expect(page.getByText(movie.Name, { exact: true }).first()).toBeVisible({ timeout: 20_000 });
  await page.waitForLoadState('networkidle');

  expect(failedResponses).toEqual([]);
});

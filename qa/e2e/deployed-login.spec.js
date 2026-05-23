const { test, expect } = require('@playwright/test');

test('deployed server login page loads without spinner deadlock', async ({ page, request, baseURL }) => {
  test.skip(process.env.JELLYRIN_E2E_DEPLOYED !== '1', 'Only runs against an already deployed server');

  const publicInfoResponse = await request.get('/System/Info/Public');
  expect(publicInfoResponse.ok()).toBeTruthy();
  const publicInfo = await publicInfoResponse.json();
  expect(publicInfo.StartupWizardCompleted).toBe(true);

  const usersResponse = await request.get('/users/public');
  expect(usersResponse.ok()).toBeTruthy();
  const users = await usersResponse.json();
  expect(users.some(user => user.Name === 'admin')).toBe(true);

  const loginUrl = `${baseURL}/web/#/login?serverid=${publicInfo.Id}&url=%2Fhome`;
  await page.goto(loginUrl);
  await expect(page.getByRole('heading', { name: 'Please sign in' }).first()).toBeVisible();
  await expect(page.getByRole('button', { name: 'Manual Login' })).toBeVisible();
});

test('deployed server authenticates and reaches empty home cleanly', async ({ page, request, baseURL }) => {
  test.skip(process.env.JELLYRIN_E2E_DEPLOYED !== '1', 'Only runs against an already deployed server');
  test.skip(!process.env.JELLYRIN_E2E_ADMIN_PASSWORD, 'Requires JELLYRIN_E2E_ADMIN_PASSWORD');

  const publicInfo = await (await request.get('/System/Info/Public')).json();
  const failedResponses = [];
  page.on('response', response => {
    if (response.status() >= 400 && !response.url().includes('/Branding/Splashscreen')) {
      failedResponses.push(`${response.status()} ${response.url()}`);
    }
  });

  await page.goto(`${baseURL}/web/#/login?serverid=${publicInfo.Id}&url=%2Fhome`);
  await page.getByRole('button', { name: 'Manual Login' }).click();
  await page.locator('#txtManualName').fill(process.env.JELLYRIN_E2E_ADMIN_USER || 'admin');
  await page.locator('#txtManualPassword').fill(process.env.JELLYRIN_E2E_ADMIN_PASSWORD);

  const authResponse = page.waitForResponse(response =>
    response.url().toLowerCase().includes('/users/authenticatebyname') && response.status() === 200
  );
  await page.locator('.manualLoginForm .button-submit').click();
  await authResponse;
  await expect(page).toHaveURL(/\/web\/#\/home/);
  await page.waitForLoadState('networkidle');

  expect(failedResponses).toEqual([]);
});

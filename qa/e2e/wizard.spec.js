const { test, expect } = require('@playwright/test');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

test('fresh install completes the Jellyfin web startup wizard and loads a scanned library', async ({ page, request, baseURL }, testInfo) => {
  const mediaDir = await fs.mkdtemp(path.join(os.tmpdir(), `jellyrin-e2e-media-${testInfo.workerIndex}-`));
  await fs.writeFile(path.join(mediaDir, 'Example Movie.mp4'), Buffer.from('fake video'));
  const failedResponses = [];
  page.on('response', response => {
    const url = response.url();
    if (response.status() >= 400 && !url.includes('/Branding/Splashscreen')) {
      failedResponses.push(`${response.status()} ${url}`);
    }
  });

  await page.goto('/web/#/wizard/start');

  await expect(page.locator('#txtServerName')).toBeVisible();
  await page.locator('#txtServerName').fill('Jellyrin QA');
  await page.locator('#selectLocalizationLanguage').selectOption('es-ES');
  await page.locator('.wizardStartForm .button-submit').click();

  await expect(page.locator('#txtUsername')).toBeVisible();
  await page.locator('#txtUsername').fill('admin');
  await page.locator('#txtManualPassword').fill('qa-secret-123');
  await page.locator('#txtPasswordConfirm').fill('qa-secret-123');
  await page.locator('.wizardUserForm .button-submit').click();

  await expect(page.locator('#divVirtualFolders')).toBeVisible();
  await expect(page.locator('#addLibrary')).toBeVisible();
  await expect((await request.get('/Environment/Drives')).ok()).toBeTruthy();
  await expect((await request.get('/Environment/DirectoryContents?Path=/&IncludeFiles=false')).ok()).toBeTruthy();
  await expect((await request.post('/Environment/ValidatePath', { data: { Path: '/' } })).ok()).toBeTruthy();
  await expect((await request.post(`/Library/VirtualFolders?name=QA%20Movies&collectionType=movies&paths=${encodeURIComponent(mediaDir)}`)).ok()).toBeTruthy();
  const virtualFolders = await (await request.get('/Library/VirtualFolders')).json();
  expect(virtualFolders).toEqual(expect.arrayContaining([
    expect.objectContaining({
      Name: 'QA Movies',
      CollectionType: 'movies',
      Locations: expect.arrayContaining([mediaDir])
    })
  ]));
  const movieFolder = virtualFolders.find(folder => folder.Name === 'QA Movies');
  await page.locator('#wizardLibraryPage .button-submit').click();

  await expect(page.locator('#selectLanguage')).toBeVisible();
  await expect(page.locator('#selectLanguage option[value="es"]')).toHaveCount(1);
  await expect(page.locator('#selectCountry option[value="ES"]')).toHaveCount(1);
  await page.locator('#selectLanguage').selectOption('es');
  await page.locator('#selectCountry').selectOption('ES');
  await page.locator('#wizardSettingsPage .button-submit').click();

  await expect(page.locator('#chkRemoteAccess')).toBeVisible();
  await page.locator('#chkRemoteAccess').locator('xpath=ancestor::form').locator('.button-submit').click();

  await expect(page.locator('#wizardFinishPage .btnWizardNext')).toBeVisible();
  await page.locator('#wizardFinishPage .btnWizardNext').click();

  await expect.poll(async () => {
    const response = await request.get('/System/Info/Public');
    const body = await response.json();
    return body.StartupWizardCompleted;
  }).toBe(true);

  const systemInfo = await (await request.get('/System/Info/Public')).json();
  await page.goto(`/web/#/login?serverid=${systemInfo.Id}&url=%2Fhome`);
  await expect(page.getByRole('heading', { name: 'Please sign in' }).first()).toBeVisible();
  await expect(page.getByRole('button', { name: 'Manual Login' })).toBeVisible();

  await page.getByRole('button', { name: 'Manual Login' }).click();
  await expect(page.locator('#txtManualName')).toBeVisible();
  await page.locator('#txtManualName').fill('admin');
  await page.locator('#txtManualPassword').fill('qa-secret-123');

  const authResponse = page.waitForResponse(response =>
    response.url().toLowerCase().includes('/users/authenticatebyname') && response.status() === 200
  );
  await page.locator('.manualLoginForm .button-submit').click();
  await authResponse;
  await expect(page).toHaveURL(/\/web\/#\/home/);

  const apiAuthResponse = await request.post('/Users/AuthenticateByName', {
    headers: {
      Authorization: 'MediaBrowser Client="Jellyfin Web", Device="Playwright", DeviceId="wizard-library", Version="dev"',
    },
    data: { Username: 'admin', Pw: 'qa-secret-123' },
  });
  expect(apiAuthResponse.ok()).toBeTruthy();
  const auth = await apiAuthResponse.json();
  const itemsResponse = await request.get(`/Users/${auth.User.Id}/Items?ParentId=${movieFolder.ItemId}&IncludeItemTypes=Movie`, {
    headers: { 'X-Emby-Token': auth.AccessToken },
  });
  expect(itemsResponse.ok()).toBeTruthy();
  const items = await itemsResponse.json();
  expect(items.TotalRecordCount).toBe(1);
  expect(items.Items[0].Name).toBe('Example Movie');

  const libraryLink = page.locator('a.itemAction[title="QA Movies"]').first();
  await expect(libraryLink).toBeVisible({ timeout: 20_000 });
  await libraryLink.click();
  await page.waitForLoadState('networkidle');
  expect(failedResponses).toEqual([]);
});

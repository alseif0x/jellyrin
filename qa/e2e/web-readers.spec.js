const { test, expect } = require('@playwright/test');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

const ADMIN_USER = 'reader-admin';
const ADMIN_PASSWORD = 'reader-qa-secret-123';
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGMAAQAA' +
  'BQABDQottAAAAABJRU5ErkJggg==',
  'base64',
);

test('hardened Jellyfin Web renders the photo slideshow and multi-page comics reader', async ({ page, request }, testInfo) => {
  test.setTimeout(120_000);
  const mediaRoot = await fs.mkdtemp(path.join(os.tmpdir(), `jellyrin-reader-e2e-${testInfo.workerIndex}-`));
  const photoDir = path.join(mediaRoot, 'photos');
  const bookDir = path.join(mediaRoot, 'books');
  await fs.mkdir(photoDir);
  await fs.mkdir(bookDir);
  await fs.writeFile(path.join(photoDir, 'Reader Photo.png'), PNG);
  await fs.writeFile(
    path.join(bookDir, 'Reader Comic.cbz'),
    zipStored([
      ['001.png', PNG],
      ['002.png', PNG],
      ['003.png', PNG],
    ]),
  );

  const pageErrors = [];
  page.on('pageerror', error => pageErrors.push(error.message));

  try {
    await completeWizard(page, request, photoDir, bookDir);
    const auth = await authenticate(request);

    const refresh = await request.post('/Library/Refresh', {
      headers: { 'X-Emby-Token': auth.AccessToken },
    });
    expect(refresh.status()).toBe(204);

    const photo = await findOnlyItem(request, auth, 'Photo', 'Photo');
    const comic = await findOnlyItem(request, auth, 'Book', 'Book');
    expect(photo.Path).toBe(path.join(photoDir, 'Reader Photo.png'));
    expect(comic.Path).toBe(path.join(bookDir, 'Reader Comic.cbz'));
    expect(comic.Container).toBe('cbz');

    await loginThroughWeb(page, auth);
    await verifyPhotoSlideshow(page, photo);
    await verifyComicsReader(page, comic);

    expect(pageErrors).toEqual([]);
  } finally {
    await fs.rm(mediaRoot, { recursive: true, force: true });
  }
});

async function completeWizard(page, request, photoDir, bookDir) {
  await page.goto('/web/#/wizard/start');

  await expect(page.locator('#txtServerName')).toBeVisible();
  await page.locator('#txtServerName').fill('Jellyrin Reader QA');
  await page.locator('.wizardStartForm .button-submit').click();

  await expect(page.locator('#txtUsername')).toBeVisible();
  await page.locator('#txtUsername').fill(ADMIN_USER);
  await page.locator('#txtManualPassword').fill(ADMIN_PASSWORD);
  await page.locator('#txtPasswordConfirm').fill(ADMIN_PASSWORD);
  await page.locator('.wizardUserForm .button-submit').click();

  await expect(page.locator('#divVirtualFolders')).toBeVisible();
  const photos = await request.post(
    `/Library/VirtualFolders?name=Reader%20Photos&collectionType=photos&paths=${encodeURIComponent(photoDir)}`,
  );
  expect(photos.status()).toBe(204);
  const books = await request.post(
    `/Library/VirtualFolders?name=Reader%20Books&collectionType=books&paths=${encodeURIComponent(bookDir)}`,
  );
  expect(books.status()).toBe(204);
  await page.locator('#wizardLibraryPage .button-submit').click();

  await expect(page.locator('#selectLanguage')).toBeVisible();
  await page.locator('#wizardSettingsPage .button-submit').click();
  await expect(page.locator('#chkRemoteAccess')).toBeVisible();
  await page.locator('#chkRemoteAccess').locator('xpath=ancestor::form').locator('.button-submit').click();
  await expect(page.locator('#wizardFinishPage .btnWizardNext')).toBeVisible();
  await page.locator('#wizardFinishPage .btnWizardNext').click();

  await expect.poll(async () => {
    const response = await request.get('/System/Info/Public');
    return (await response.json()).StartupWizardCompleted;
  }).toBe(true);
}

async function authenticate(request) {
  const response = await request.post('/Users/AuthenticateByName', {
    headers: {
      Authorization: 'MediaBrowser Client="Jellyfin Web", Device="Playwright", DeviceId="web-readers", Version="dev"',
    },
    data: { Username: ADMIN_USER, Pw: ADMIN_PASSWORD },
  });
  expect(response.status()).toBe(200);
  return response.json();
}

async function findOnlyItem(request, auth, itemType, mediaType) {
  const response = await request.get(
    `/Items?UserId=${auth.User.Id}&Recursive=true&IncludeItemTypes=${itemType}&MediaTypes=${mediaType}&StartIndex=0&Limit=10`,
    { headers: { 'X-Emby-Token': auth.AccessToken } },
  );
  expect(response.status()).toBe(200);
  const body = await response.json();
  expect(body.TotalRecordCount).toBe(1);
  return body.Items[0];
}

async function loginThroughWeb(page, auth) {
  const publicInfo = await (await page.request.get('/System/Info/Public')).json();
  await page.goto(`/web/#/login?serverid=${publicInfo.Id}&url=%2Fhome`);
  const manualName = page.locator('#txtManualName');
  const manualFormVisible = await manualName.waitFor({ state: 'visible', timeout: 5_000 })
    .then(() => true)
    .catch(() => false);
  if (!manualFormVisible) {
    const manualLogin = page.getByRole('button', { name: 'Manual Login' });
    await manualLogin.waitFor({ state: 'visible', timeout: 15_000 });
    await manualLogin.click();
  }
  await manualName.fill(ADMIN_USER);
  await page.locator('#txtManualPassword').fill(ADMIN_PASSWORD);
  const authenticated = page.waitForResponse(response =>
    response.url().toLowerCase().includes('/users/authenticatebyname') && response.status() === 200,
  );
  await page.locator('.manualLoginForm .button-submit').click();
  await authenticated;
  await expect(page).toHaveURL(/\/web\/#\/home/);
  expect(auth.AccessToken).toBeTruthy();
}

async function visiblePlayButton(page) {
  const button = page.locator('.btnPlay:not(.hide), .btnReplay:not(.hide)').first();
  await button.waitFor({ state: 'visible', timeout: 20_000 });
  return button;
}

async function verifyPhotoSlideshow(page, photo) {
  await page.goto(`/web/#/details?id=${photo.Id}`);
  await page.waitForLoadState('networkidle').catch(() => {});
  const download = page.waitForResponse(response =>
    response.url().includes(`/Items/${photo.Id}/Download`) && response.status() === 200,
  );
  await (await visiblePlayButton(page)).click();
  await download;

  const dialog = page.locator('.slideshowDialog').filter({ hasNot: page.locator('#comicsPlayer') }).last();
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.slideshowSwiperContainer')).toHaveClass(/swiper-initialized/);
  const activeImage = dialog.locator('.swiper-slide-active img');
  await expect.poll(() => activeImage.evaluate(image => ({
    complete: image.complete,
    width: image.naturalWidth,
    source: image.currentSrc || image.src,
  }))).toEqual(expect.objectContaining({
    complete: true,
    width: 1,
  }));
  expect(await activeImage.getAttribute('src')).toContain(`/Items/${photo.Id}/Download`);

  const pause = dialog.locator('.btnSlideshowPause').first();
  // Jellyfin hides the desktop OSD after three seconds. Dispatching the same DOM event keeps
  // this assertion independent of that cosmetic timer while exercising the bound handler.
  await pause.dispatchEvent('click');
  await expect(pause.locator('.material-icons')).toHaveClass(/pause/);
  await pause.dispatchEvent('click');
  await expect(pause.locator('.material-icons')).toHaveClass(/play_arrow/);
  await dialog.locator('.btnSlideshowExit').dispatchEvent('click');
  await expect(dialog).toBeHidden();
}

async function verifyComicsReader(page, comic) {
  await page.goto(`/web/#/details?id=${comic.Id}`);
  await page.waitForLoadState('networkidle').catch(() => {});
  const download = page.waitForResponse(response =>
    response.url().includes(`/Items/${comic.Id}/Download`) && response.status() === 200,
  );
  const worker = page.waitForResponse(response =>
    response.url().includes('/libraries/worker-bundle.js') && response.status() === 200,
  );
  await (await visiblePlayButton(page)).click();
  await Promise.all([download, worker]);

  const reader = page.locator('#comicsPlayer');
  await expect(reader).toBeVisible();
  await expect(reader.locator('.slideshowSwiperContainer')).toHaveClass(/swiper-initialized/);
  await expect(reader.locator('.swiper-pagination-total')).toHaveText('3');
  await expect(reader.locator('.swiper-pagination-current')).toHaveText('1');
  const activeImage = reader.locator('.swiper-slide-active img');
  await expect.poll(() => activeImage.evaluate(image => image.complete && image.naturalWidth)).toBe(1);

  await reader.locator('.swiper-button-next').click();
  await expect(reader.locator('.swiper-pagination-current')).toHaveText('2');
  await expect(reader.locator('.swiper-slide-active')).toHaveAttribute('data-swiper-slide-index', '1');
  await expect.poll(() => reader.locator('.swiper-slide-active img').evaluate(image => image.complete && image.naturalWidth)).toBe(1);

  const direction = reader.locator('.btnToggleLangDir');
  await expect(direction).toHaveAttribute('title', 'Right To Left');
  await direction.click();
  await expect(reader.locator('.slideshowSwiperContainer')).toHaveAttribute('dir', 'rtl');
  await expect(direction).toHaveAttribute('title', 'Left To Right');
  await expect(reader.locator('.swiper-pagination-current')).toHaveText('2');
  await expect(reader.locator('.swiper-slide-active')).toHaveAttribute('data-swiper-slide-index', '1');

  const view = reader.locator('.btnToggleView');
  await expect(view).toHaveAttribute('title', 'Double Page View');
  await view.click();
  await expect(view).toHaveAttribute('title', 'Single Page View');
  // Two-page mode groups the three pages into two pagination groups. Swiper anchors the first
  // group at page 1, while the previously selected page 2 remains one of the visible slides.
  await expect(reader.locator('.swiper-slide-visible[data-swiper-slide-index="1"]')).toHaveCount(1);
  await expect(reader.locator('.swiper-pagination-current')).toHaveText('1');
  await expect(reader.locator('.swiper-pagination-total')).toHaveText('2');

  await reader.locator('.btnExit').click();
  await expect(reader).toBeHidden();
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function zipStored(entries) {
  const localRecords = [];
  const centralRecords = [];
  let offset = 0;
  for (const [name, payload] of entries) {
    const nameBytes = Buffer.from(name);
    const checksum = crc32(payload);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 6);
    local.writeUInt16LE(0, 8);
    local.writeUInt16LE(0, 10);
    local.writeUInt16LE(33, 12);
    local.writeUInt32LE(checksum, 14);
    local.writeUInt32LE(payload.length, 18);
    local.writeUInt32LE(payload.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    local.writeUInt16LE(0, 28);
    localRecords.push(local, nameBytes, payload);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0, 8);
    central.writeUInt16LE(0, 10);
    central.writeUInt16LE(0, 12);
    central.writeUInt16LE(33, 14);
    central.writeUInt32LE(checksum, 16);
    central.writeUInt32LE(payload.length, 20);
    central.writeUInt32LE(payload.length, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt16LE(0, 30);
    central.writeUInt16LE(0, 32);
    central.writeUInt16LE(0, 34);
    central.writeUInt16LE(0, 36);
    central.writeUInt32LE(0, 38);
    central.writeUInt32LE(offset, 42);
    centralRecords.push(central, nameBytes);
    offset += local.length + nameBytes.length + payload.length;
  }
  const centralDirectory = Buffer.concat(centralRecords);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralDirectory.length, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20);
  return Buffer.concat([...localRecords, centralDirectory, end]);
}

const { defineConfig, devices } = require('@playwright/test');
const path = require('node:path');

const port = Number(process.env.JELLYRIN_E2E_PORT || 18097);
const baseURL = process.env.JELLYRIN_E2E_BASE_URL || `http://127.0.0.1:${port}`;
const dataRoot = process.env.JELLYRIN_E2E_DATA_ROOT || `/tmp/jellyrin-e2e-${process.pid}`;
const webServerEnabled = process.env.JELLYRIN_E2E_NO_WEBSERVER !== '1';
const chromiumExecutable = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE;
const webDir = process.env.JELLYRIN_E2E_WEB_DIR || path.join(__dirname, 'web');
const serverCommand = process.env.JELLYRIN_E2E_SERVER_COMMAND || 'cargo run -p jellyrin-server --';

const config = {
  testDir: './qa/e2e',
  timeout: 60_000,
  expect: {
    timeout: 10_000
  },
  use: {
    baseURL,
    launchOptions: chromiumExecutable ? { executablePath: chromiumExecutable } : {},
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure'
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] }
    }
  ]
};

if (webServerEnabled) {
  config.webServer = {
    command: [
      serverCommand,
      `--host 127.0.0.1 --port ${port}`,
      `--data-dir ${dataRoot}/data`,
      `--config-dir ${dataRoot}/config`,
      `--cache-dir ${dataRoot}/cache`,
      `--log-dir ${dataRoot}/logs`,
      `--web-dir ${JSON.stringify(webDir)}`
    ].join(' '),
    url: `${baseURL}/health`,
    reuseExistingServer: false,
    timeout: 120_000
  };
}

module.exports = defineConfig(config);

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
const test = require('node:test');

const {
  PLUGIN_ID,
  buildPayload,
  validateConfig,
  verifyPersistedTuner,
} = require('./magstv-configure-jellyrin');

const validInput = {
  username: 'operator@example.test',
  password: 'not-a-real-password',
};

test('builds the fixed MAGSTV plugin route and bounded defaults', () => {
  const payload = buildPayload(validateConfig(validInput));
  assert.equal(payload.Id, 'magstv');
  assert.equal(payload.Type, `plugin:${PLUGIN_ID}`);
  assert.equal(payload.PluginId, PLUGIN_ID);
  assert.equal(payload.PageSize, 200);
  assert.equal(payload.TunerCount, 1);
  assert.equal('BootstrapUrl' in payload, false);
  assert.equal('EgressIp' in payload, false);
});

test('requires only username and password', () => {
  assert.doesNotThrow(() => validateConfig(validInput));
  assert.throws(() => validateConfig({ username: '', password: validInput.password }));
  assert.throws(() => validateConfig({ username: validInput.username, password: '' }));
});

test('accepts only persisted tuners with no plaintext credentials and an encrypted reference', () => {
  const payload = buildPayload(validateConfig(validInput));
  const persisted = {
    ...payload,
    Username: undefined,
    Password: undefined,
    JellyrinProviderSecretRef: {
      Id: 'ps_fixture',
      Provider: `plugin-${PLUGIN_ID}`,
      Revision: 1,
    },
  };
  delete persisted.Username;
  delete persisted.Password;
  assert.doesNotThrow(() => verifyPersistedTuner(persisted, payload));
  assert.throws(() => verifyPersistedTuner({ ...persisted, Password: 'reflected' }, payload));
  assert.throws(() => verifyPersistedTuner({ ...persisted, JellyrinProviderSecretRef: undefined }, payload));
});

test('validate-only output never prints token, credentials, or bootstrap URL', () => {
  const script = path.join(__dirname, 'magstv-configure-jellyrin.js');
  const secrets = {
    token: 'admin-token-must-not-print',
    username: 'username-must-not-print',
    password: 'password-must-not-print',
  };
  const result = spawnSync(process.execPath, [script, '--validate-only'], {
    encoding: 'utf8',
    env: {
      ...process.env,
      JELLYRIN_BASE_URL: 'https://jellyrin.example.test',
      JELLYRIN_API_TOKEN: secrets.token,
      JELLYRIN_MAGSTV_USERNAME: secrets.username,
      JELLYRIN_MAGSTV_PASSWORD: secrets.password,
    },
  });
  assert.equal(result.status, 0, result.stderr);
  const combined = `${result.stdout}\n${result.stderr}`;
  for (const secret of Object.values(secrets)) {
    assert.equal(combined.includes(secret), false);
  }
  assert.match(result.stdout, /"status": "magstv-input-valid"/);
});

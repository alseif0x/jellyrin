# MAGSTV 0.3.1 redeploy and deployed QA

This runbook upgrades the external-process package without deleting the existing tuner or its
encrypted provider-secret reference. It then runs exactly one Settings submission. Only one
MAGSTV configuration/import process may run at a time.

## Safety gates

- Do not uninstall the plugin or delete the `magstv` tuner during an upgrade.
- Do not redeploy while a catalogue import or this Playwright suite is running.
- Do not click `Actualizar catálogo` during the first-run Settings test. `Guardar e indexar`
  already starts the VOD import.
- Keep the administrator token and the two account fields in separate mode-0600 files. Never put
  them in command arguments, repository JSON, logs, or Git.
- If the Settings submission has succeeded and the QA process later stops, use the verify-only
  recovery below. Never submit the account again merely to resume polling.

Before changing the deployment, require clean, committed source and record both exact revisions:

```bash
core_repo=/home/ubuntu/projects/jellyrin
plugin_repo=/home/ubuntu/projects/jellyrin-plugin-magstv

test -z "$(git -C "$core_repo" status --porcelain)"
test -z "$(git -C "$plugin_repo" status --porcelain)"
git -C "$core_repo" rev-parse HEAD
git -C "$plugin_repo" rev-parse HEAD

if pgrep -af '[p]laywright.*[d]eployed-magstv-plugin|[d]eployed-magstv-plugin\.spec\.js'; then
  echo 'MAGSTV deployed QA is already running; do not start another run' >&2
  exit 1
fi
```

If a previous Settings save may still be importing VOD, stop here and use the verify-only flow;
package installation deliberately takes the plugin lifecycle writer and must not race that import.

## Build and verify the arm64 package

Build version `0.3.1` from the committed plugin tree. The script enforces the Cargo/manifest
version and writes the checksum into the generated repository manifest.

```bash
cd "$plugin_repo"
test "$(jq -r .Version packaging/manifest.json)" = 0.3.1
MAGSTV_RUNTIME_TARGET=aarch64-unknown-linux-gnu ./scripts/package-runtime.sh

package_zip="$plugin_repo/dist/jellyrin-magstv-runtime-0.3.1-aarch64-unknown-linux-gnu.zip"
package_sum="$package_zip.sha256"
package_repository="$plugin_repo/dist/jellyrin-magstv-repository-0.3.1-aarch64-unknown-linux-gnu.json"

(cd "$(dirname "$package_zip")" && sha256sum -c "$(basename "$package_sum")")
unzip -tq "$package_zip"
test "$(unzip -p "$package_zip" manifest.json | jq -r .Version)" = 0.3.1
jq -e \
  'length == 1 and .[0].Guid == "7a7a8541-29f8-4c35-99b1-66df55f8399e"
   and .[0].Versions[0].Version == "0.3.1"
   and (.[0].Versions[0].Checksum | startswith("sha256:"))' \
  "$package_repository" >/dev/null
```

## Stage and install without losing Settings

The example below uses Jellyrin's private local repository directory. It preserves every existing
package repository, adds or replaces only `Mags QA local`, installs through the normal package API,
grants the two declared permissions, and enables the new runtime. Adjust only `api_base`,
`api_token_file`, and the repository directory if the deployment differs.

```bash
api_base=http://127.0.0.1:8096
api_token_file=/secure/path/jellyrin-admin-token
plugin_id=7a7a8541-29f8-4c35-99b1-66df55f8399e
repository_dir=/srv/jellyrin/plugin-repository
qa_state_dir=$(mktemp -d /tmp/jellyrin-magstv-redeploy.XXXXXX)
chmod 0700 "$qa_state_dir"

test -f "$api_token_file"
test "$(stat -c '%a' "$api_token_file")" = 600
IFS= read -r qa_api_token <"$api_token_file"
case "$qa_api_token" in
  ''|*[!A-Za-z0-9._-]*) echo 'unsupported administrator token format' >&2; exit 1 ;;
esac
qa_curl_config="$qa_state_dir/curl.conf"
umask 077
{
  printf '%s\n' 'silent' 'show-error' 'fail-with-body'
  printf 'header = "X-Emby-Token: %s"\n' "$qa_api_token"
} >"$qa_curl_config"
unset qa_api_token

curl --config "$qa_curl_config" \
  "$api_base/System/Configuration/livetv" \
  --output "$qa_state_dir/livetv.before.json"
jq -e \
  '.TunerHosts | any(.Id == "magstv"
    and (.JellyrinProviderSecretRef.Id | type == "string" and length > 0)
    and (.JellyrinProviderSecretRef.Revision | tonumber > 0))' \
  "$qa_state_dir/livetv.before.json" >/dev/null
jq -S \
  '.TunerHosts[] | select(.Id == "magstv")
   | {Type, FriendlyName, JellyrinProviderSecretRef, PersistedChannelCount}' \
  "$qa_state_dir/livetv.before.json" >"$qa_state_dir/tuner.before.json"

sudo install -d -m 0755 "$repository_dir"
sudo install -m 0644 "$package_zip" "$repository_dir/$(basename "$package_zip")"
jq --arg source "file://$repository_dir/$(basename "$package_zip")" \
  '.[0].Versions[0].SourceUrl = $source' \
  "$package_repository" >"$qa_state_dir/mags-repository.json"
sudo install -m 0644 "$qa_state_dir/mags-repository.json" \
  "$repository_dir/mags-repository-0.3.1.json"

curl --config "$qa_curl_config" \
  "$api_base/Package/Repositories" \
  --output "$qa_state_dir/repositories.before.json"
jq --arg url "file://$repository_dir/mags-repository-0.3.1.json" \
  '[.[] | {
      Name,
      Url,
      Enabled: (if has("Enabled") then .Enabled else true end)
    }]
   | map(select(.Name != "Mags QA local"))
   + [{Name: "Mags QA local", Url: $url, Enabled: true}]' \
  "$qa_state_dir/repositories.before.json" >"$qa_state_dir/repositories.next.json"
curl --config "$qa_curl_config" \
  --request POST \
  --header 'Content-Type: application/json' \
  --data-binary "@$qa_state_dir/repositories.next.json" \
  "$api_base/Package/Repositories" \
  --output /dev/null
curl --config "$qa_curl_config" \
  --request POST \
  "$api_base/Package/Repositories/Refresh" \
  --output /dev/null

curl --config "$qa_curl_config" \
  --request POST \
  "$api_base/Package/Packages/Installed/Mags?Version=0.3.1" \
  --output /dev/null
curl --config "$qa_curl_config" \
  "$api_base/Packages/Installing/$plugin_id" \
  --output "$qa_state_dir/install-status.json"
jq -e \
  '.Status == "Completed" and .Phase == "Completed"
   and .Result.Version == "0.3.1"' \
  "$qa_state_dir/install-status.json" >/dev/null

printf '%s\n' '{"Permissions":["Network","ProviderSecrets"]}' \
  >"$qa_state_dir/permissions.json"
curl --config "$qa_curl_config" \
  --request POST \
  --header 'Content-Type: application/json' \
  --data-binary "@$qa_state_dir/permissions.json" \
  "$api_base/Plugins/$plugin_id/Permissions" \
  --output /dev/null
curl --config "$qa_curl_config" \
  --request POST \
  "$api_base/Plugins/$plugin_id/0.3.1/Enable" \
  --output /dev/null

curl --config "$qa_curl_config" \
  "$api_base/Plugins/$plugin_id/Health" \
  --output "$qa_state_dir/plugin-health.json"
jq -e \
  '.Version == "0.3.1" and .Status == "Active" and .LastError == null
   and (.RuntimeInstances | any(.Status == "Active" and .Health.Status == "Healthy"))' \
  "$qa_state_dir/plugin-health.json" >/dev/null

curl --config "$qa_curl_config" \
  "$api_base/System/Configuration/livetv" \
  --output "$qa_state_dir/livetv.after.json"
jq -S \
  '.TunerHosts[] | select(.Id == "magstv")
   | {Type, FriendlyName, JellyrinProviderSecretRef, PersistedChannelCount}' \
  "$qa_state_dir/livetv.after.json" >"$qa_state_dir/tuner.after.json"
cmp --silent "$qa_state_dir/tuner.before.json" "$qa_state_dir/tuner.after.json"
```

The final `cmp` is the no-settings-loss gate: the encrypted reference, its revision, tuner type,
friendly name, and persisted channel count must survive package replacement unchanged.

## Run Settings once and monitor the same import

The provider environment file must contain only `JELLYRIN_MAGSTV_USERNAME` and
`JELLYRIN_MAGSTV_PASSWORD`. Set an explicit four-hour catalogue timeout even though four hours is
also the test default. Keep `CLICK_REFRESH=0`.

```bash
cd "$core_repo"
set -a
. /secure/path/magstv-e2e-credentials.env
set +a

JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_MAGSTV_QA=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_E2E_API_TOKEN_FILE="$api_token_file" \
JELLYRIN_E2E_MAGSTV_CLICK_REFRESH=0 \
JELLYRIN_E2E_MAGSTV_SYNC_TIMEOUT_MS=14400000 \
PLAYWRIGHT_CHROMIUM_EXECUTABLE=/home/ubuntu/.cache/ms-playwright/chromium-1208/chrome-linux/chrome \
npm run test:e2e:magstv-plugin

unset JELLYRIN_MAGSTV_USERNAME JELLYRIN_MAGSTV_PASSWORD
```

The suite itself emits `magstv-sync-pending` JSON every 30 seconds. Let that single process poll;
do not open another configuration test, call the VOD refresh endpoint, or create a parallel
provider login. A pass requires all of the following in one `magstv-e2e-passed` result:

- at least 1,000 live channels;
- at least 30,000 movies;
- at least 20,000 series and 100,001 episodes;
- visible and openable `Mags Movies`, `Mags Series`, and `Mags Live TV` views;
- non-empty media bytes from one live channel, one movie, and one episode;
- no browser chunk-loading or page errors.

## Resume after a test timeout without another Settings save

Use this only after the first process is gone and the encrypted `magstv` tuner reference exists.
Do not source the provider credential file. This mode visits Settings read-only, validates that it
still exposes only username/password, waits on the already-started catalogue, and then performs
the same three-view and playback checks.

```bash
cd "$core_repo"
JELLYRIN_E2E_DEPLOYED=1 \
JELLYRIN_E2E_MAGSTV_QA=1 \
JELLYRIN_E2E_MAGSTV_VERIFY_ONLY=1 \
JELLYRIN_E2E_NO_WEBSERVER=1 \
JELLYRIN_E2E_BASE_URL=https://jellyrin.test.kode.live \
JELLYRIN_E2E_API_TOKEN_FILE="$api_token_file" \
JELLYRIN_E2E_MAGSTV_CLICK_REFRESH=0 \
JELLYRIN_E2E_MAGSTV_SYNC_TIMEOUT_MS=14400000 \
PLAYWRIGHT_CHROMIUM_EXECUTABLE=/home/ubuntu/.cache/ms-playwright/chromium-1208/chrome-linux/chrome \
npm run test:e2e:magstv-plugin
```

After QA, remove only the runbook-created temporary files. Retain the secure token/credential files
if they are operator-managed; delete them separately only if they were created expressly for this
run.

```bash
rm -f \
  "$qa_state_dir/curl.conf" \
  "$qa_state_dir/livetv.before.json" \
  "$qa_state_dir/livetv.after.json" \
  "$qa_state_dir/tuner.before.json" \
  "$qa_state_dir/tuner.after.json" \
  "$qa_state_dir/repositories.before.json" \
  "$qa_state_dir/repositories.next.json" \
  "$qa_state_dir/mags-repository.json" \
  "$qa_state_dir/permissions.json" \
  "$qa_state_dir/install-status.json" \
  "$qa_state_dir/plugin-health.json"
rmdir "$qa_state_dir"
```

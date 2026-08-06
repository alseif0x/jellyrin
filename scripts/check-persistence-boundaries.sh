#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

violations=0

contract_leaks="$(
  rg -n '(^|[^[:alnum:]_])(sqlx|jellyrin-db|jellyrin-api)([^[:alnum:]_]|$)' \
    crates/jellyrin-persistence/Cargo.toml crates/jellyrin-persistence/src || true
)"
if [[ -n "$contract_leaks" ]]; then
  echo "jellyrin-persistence must remain independent from drivers and application crates:"
  echo "$contract_leaks"
  violations=1
fi

api_production_leaks="$(
  awk '/^mod tests \{$/{exit} {print}' crates/jellyrin-api/src/lib.rs \
    | rg -n 'sqlx::|\.pool\(\)' || true
)"
if [[ -n "$api_production_leaks" ]]; then
  echo "production API code must use persistence contracts instead of SQLx or raw pools:"
  echo "$api_production_leaks"
  violations=1
fi

api_dependency_leaks="$(
  awk '/^\[dev-dependencies\]$/{exit} {print}' crates/jellyrin-api/Cargo.toml \
    | rg -n '^sqlx([.]workspace)?[[:space:]]*=' || true
)"
if [[ -n "$api_dependency_leaks" ]]; then
  echo "sqlx may only be a jellyrin-api dev-dependency while legacy tests still need it:"
  echo "$api_dependency_leaks"
  violations=1
fi

sqlite_adapter_dependency_leaks="$(
  rg -n '^jellyrin-(api|db)[[:space:]]*=' \
    crates/jellyrin-persistence-sqlite/Cargo.toml || true
)"
if [[ -n "$sqlite_adapter_dependency_leaks" ]]; then
  echo "the SQLite adapter must depend inward on contracts, never on API or jellyrin-db:"
  echo "$sqlite_adapter_dependency_leaks"
  violations=1
fi

legacy_named_configuration_sql="$(
  rg -n 'FROM (named_configurations|system_configuration_payloads)|INTO (named_configurations|system_configuration_payloads)|UPDATE (named_configurations|system_configuration_payloads)|DELETE FROM (named_configurations|system_configuration_payloads)' \
    crates/jellyrin-db/src || true
)"
if [[ -n "$legacy_named_configuration_sql" ]]; then
  echo "configuration SQL belongs in jellyrin-persistence-sqlite:"
  echo "$legacy_named_configuration_sql"
  violations=1
fi

legacy_user_repository_sql="$(
  rg -n 'user_configurations|user_passwords|\b(PasswordRow|UserConfigurationRow|UserRow)\b|INSERT INTO users|UPDATE users|DELETE FROM users|COUNT\(\*\) FROM users|SELECT id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at' \
    crates/jellyrin-db/src || true
)"
if [[ -n "$legacy_user_repository_sql" ]]; then
  echo "user profile reads and user configuration SQL belong in jellyrin-persistence-sqlite:"
  echo "$legacy_user_repository_sql"
  violations=1
fi

if [[ "$violations" -ne 0 ]]; then
  exit 1
fi

echo "Persistence dependency boundaries are clean."

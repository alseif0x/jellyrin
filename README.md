# Jellyrin

Jellyrin is a Rust port of Jellyfin server behavior. The current milestone is a
compatibility-first backend that can serve the existing Jellyfin web client and
then grow feature-by-feature against golden behavior from upstream Jellyfin.

Planning lives outside this repository:

```text
/home/cdmonio/projects/jellyrin/plans
```

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo run -p jellyrin-server -- --web-dir /home/cdmonio/dev/jellyfin-web/dist
```

The local development service is installed as `jellyrin-rust-dev.service` and
listens on port `8097` so it can run alongside the upstream .NET Jellyfin
development server on `8096`.

## Compatibility Notes

Jellyfin Web does not always call API routes with the same casing as the
canonical upstream route name. For example, the client has been observed calling
`/users/public`, `/Users/authenticatebyname`,
`/sessions/capabilities/full` and `/quickconnect/enabled`.

When adding Jellyfin-compatible endpoints, keep one handler implementation and
register the canonical route plus observed lowercase or mixed-case aliases. A
404 caused only by path casing is treated as a compatibility bug and should be
covered by Playwright or route-level regression tests.

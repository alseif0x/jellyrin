# Plugin boundary

Jellyrin's public repository contains the plugin ABI, SDK, runtime hosts, package
installation, capability dispatch, and provider-neutral tests. Individual
service integrations are distributed separately and are not part of this
repository.

## Public surface

- `jellyrin-plugin-sdk` owns stable, JSON-compatible capability contracts.
- `jellyrin-plugin-rpc` owns the transport envelope between Jellyrin and a
  plugin host.
- Runtime hosts load an installed package and dispatch only capabilities and
  permissions declared by its manifest.
- Core and API code operate on generic capability names and payloads. They must
  not depend on a service-specific plugin crate.
- Public tests use synthetic providers and sanitised fixtures.

The generic live-TV boundary is the `LiveTvProvider` capability. Provider
packages may implement channel, programme, media-sync, and playback-related
actions behind that capability without adding their protocol, identifiers, or
secrets to Jellyrin itself.

## Out-of-tree plugins

Private and vendor-specific plugins should live in a separate repository. A
plugin repository may depend on the public SDK, produce an installable package,
and maintain its own release process. Local checkouts can be placed under
`private-plugins/`, which is ignored by Git.

Do not add any of the following to the public tree:

- provider names, endpoints, application identifiers, wire field names, or
  signing implementations;
- credentials, keys, tokens, captures, signed URLs, or real catalogue data;
- built-in registration or direct API dependencies on a custom provider;
- provider-specific egress, deployment, or reverse-engineering tooling.

If an out-of-tree plugin exposes a missing general capability, extend the SDK
with provider-neutral request and response types and validate it with a fake
provider before changing the private implementation.

# Plugin boundary

Jellyrin's public repository contains the plugin ABI, SDK, runtime hosts, package
installation, capability dispatch, and provider-neutral tests. Individual
service integrations are distributed separately and are not part of this
repository.

The MAGSTV implementation is maintained in
[`alseif0x/jellyrin-plugin-magstv`](https://github.com/alseif0x/jellyrin-plugin-magstv).
Its release must pin the exact published Jellyrin SDK/RPC commit used for the
compatibility matrix; a local uncommitted core tree is not a releasable
dependency.

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

The generic on-demand library boundary is the `VodLibraryProvider` capability.
A provider package imports movies, series and episodes as bounded
`VodMediaItem` values whose `ProviderReference` is an opaque,
provider-authenticated token — never a URL — and resolves ephemeral playback
URLs just in time through the `ResolvePlayback` action. Imported items are
sanitised onto the SDK contract, persisted through the transactional remote
media catalog staging, and never carry credentials, licenses or signed URLs.
One plugin may own multiple configured tuner instances. A catalog refresh
enumerates those instances in stable tuner-id order, locks every instance, and
imports each scoped grant into the same transaction. Item metadata retains its
originating tuner id so JIT playback returns to the correct secret boundary.
The visible movie/series snapshots publish only after every instance succeeds;
an incomplete refresh leaves the previous complete snapshots visible.
VOD playback reuses the live-TV delivery contract: only `DirectProxy` of
MPEG-TS is accepted, fail-closed.

## External configuration pages

An active `ExternalProcess` package may declare a root-level `.html` or `.htm`
file in its signed manifest `WebPages` list. Jellyrin discovers that metadata
without executing the native runtime and serves only the declared regular file
from the canonical install directory. Both discovery and content routes require
an administrator; traversal, nested paths, symlinks, undeclared files and pages
larger than 1 MiB fail closed. Responses are HTML with `no-cache`, `no-store`
and `nosniff`.

These pages run in the authenticated Jellyrin dashboard origin and are therefore
part of the installed package's trusted code surface. A provider page must use
the existing authenticated API client, keep account fields write-only, and
submit credentials through the Live TV tuner boundary below. It must not read
back secrets, parse tokens from browser storage, embed operational signing keys,
or surface upstream error bodies.

## Provider secret grants

Every `ExternalProcess` package exposing `LiveTvProvider` or
`VodLibraryProvider` is classified as a sensitive-provider boundary, even when
its manifest omits `ProviderSecrets`. The generic plugin-configuration endpoint
therefore rejects provider credentials for all such packages; omitting the
permission is not a bypass.
`ProviderSecrets` is the explicit high-risk opt-in required to receive a grant:
the package must request it in the manifest and an administrator must grant it.
Jellyrin validates both sets, the external runtime, the canonical `plugin:<id>`
tuner route and the persisted secret namespace before decrypting anything.
Credentials can only enter through a Live TV tuner write that is encrypted
before the first provider invocation.

`LiveTvProviderRequest.SecretGrant` is optional for wire compatibility and is
never durable. When present, it contains zeroizing sensitive strings and scope
for exactly one plugin id, tuner id and action, plus the server-owned secret id
and revision. A provider must reject any mismatch before using the values and
should immediately transfer them into its own zeroizing credential type. It
must not copy the grant into caches, continuation tokens, catalog items,
diagnostics, environment variables or playback references.

Every invocation containing `SecretGrant` uses a one-shot process; playback and
generic secret calls use `provider-secret`, while paginated catalog import uses
the isolated `catalog-import` lane and keeps that process only across its
continuation pages. One import has a 120-second deadline, at most 256 pages,
100,000 channels (or 100,000 VOD media items), 10,000 categories, 4 KiB
continuation tokens, a 1 MiB RPC frame limit per page, and a 64 MiB aggregate
encoded-JSON budget. The aggregate
budget is a payload bound rather than an RSS guarantee because the in-memory
JSON representation has overhead. No catalog or playback grant reaches a persistent host.
Core reuses and explicitly scrubs one request value, zeroizes RPC byte
buffers, redacts request/result `Debug`, filters secret-shaped provider output,
and rejects credential canaries reflected by any grant-bearing result.

A per-plugin R/W lifecycle lock, keyed by normalized plugin identity, holds a
reader from the fresh canonical database read through invocation. Permission,
credential, tuner and plugin-lifecycle mutations hold the writer through host
invalidation, closing the revocation/rotation TOCTOU window. Normal calls take
a per-lane admission permit before the reader, then re-fetch the canonical
active plugin; native processes cannot use the unsupervised generic loader.
Non-secret hosts remain keyed by normalized identity/lane and their effective
permission fingerprint. These controls reduce
secret lifetime; they do not turn a native process under the Jellyrin Unix
account into a security sandbox or prove complete heap zeroization.

The fail-closed detector covers common credential key variants and credentials
or sensitive query parameters embedded in parseable URLs. External channel
responses are projected onto a safe schema: remote image/media-stream fields
are excluded, while public text, provider identifiers and categories are
bounded and reject controls or URL values.

Playback responses may set `RequiresProviderEgress=true`. Jellyrin then fetches
the short-lived source through the operator-controlled
`JELLYRIN_PROVIDER_EGRESS_PROXY`; it never substitutes the viewer address for a
provider egress identity. The core validates the upstream URL, disables
redirects, rejects manifest disclosure, and exposes only Jellyrin's own proxy
route to the client.

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

The MAGSTV package consumes `SecretGrant`, validates its complete scope,
removes the legacy `MAGSTV_SECRET_*` credential fallback and requires both
`Network` and `ProviderSecrets`. It now also declares `VodLibraryProvider` and
serves paginated media imports and JIT VOD playback resolution through it.
Release remains blocked until the plugin pins a
published Jellyrin SDK/RPC commit, and the cross-repository matrix plus real
credential E2E pass; a local path override is only a development aid.

MAGSTV playback status (verified 2026-08-18 against provider app 4.99.5):
catalogue import (live and VOD) works against the real portal, but every
playback resolution is rejected (`rc=1`) because the provider moved its media
plane behind an encrypted relay handshake implemented in a native library.
The runtime therefore fails closed on `ResolvePlayback`; restoring playback is
tracked as separate transport work in the consolidation handoff.

`ExternalProcess` packages are native executables, not an OS sandbox. Package
SHA-256, ABI checks, bounded extraction, an explicit environment allowlist, and
permission grants reduce accidental exposure. Database URLs, provider-vault
keys, PostgreSQL variables, and common cloud/CI credential namespaces are
unconditionally denied even when a manifest requests them. A `LiveTvProvider`
also cannot request exact account-credential-shaped names such as username,
password, API/access token, or secret-key variables; reviewed exact protocol
and device settings remain possible for controlled native packages.

On Unix each `ExternalProcess` host leads a dedicated process group. Shutdown
first attempts the RPC, then sends `SIGTERM` with a bounded grace period and
finally `SIGKILL`; timeout and `Drop` invalidate the transport, terminate the
whole group and reap its leader. The process still runs under Jellyrin's Unix
identity. Install only reproducible packages from controlled repositories;
untrusted third-party code requires a separate container or OS identity plus
signed release metadata.

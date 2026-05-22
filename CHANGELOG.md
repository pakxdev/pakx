# Changelog

All notable changes to pakx will be documented in this file.

The format roughly follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and pakx follows [Semantic Versioning](https://semver.org).

## [Unreleased]

### Changed

- **`pakx search --no-pakx` renamed to `--no-pakx-registry`** so the
  flag matches the pre-existing `--no-pakx-registry` on `pakx install`
  and `pakx test`. The three subcommands now share one flag name for
  the same source toggle. `--no-pakx` is retained as a hidden alias
  for one release; scripts continue to work without modification
  during the migration window.

### Deprecated

- **`--no-pakx` on `pakx search`** — use `--no-pakx-registry`. The
  alias will be removed in v0.2.

### Added

- **Top-level `--color <auto|always|never>` flag.** Threaded through
  every paint helper in `pakx::ui`, the new global flag joins the
  pre-existing `NO_COLOR` env-var + `IsTerminal` auto-detection as a
  third color-resolution input. `auto` (default) preserves v0.1
  behaviour. `always` force-enables ANSI codes regardless of how the
  process is invoked — useful for `pakx list --color always | less -R`
  where the pipe defeats the TTY probe. `never` force-disables for
  scripted output and CI logs that mis-render escape sequences. The
  flag is `global = true` so it works after any subcommand
  (`pakx list --color never` and `pakx --color never list` both
  parse).

- **`sponsors:` block in `SKILL.md` frontmatter (Phase X2b — see
  `pakx-registry/SPONSOR_LINKS_SPEC.md`).** Publishers can now declare
  up to 5 sponsor links per package and have them flow through `pakx
  pack` → `pakx publish` → `pakx info`. Author surface (`SKILL.md`):
  ```yaml
  sponsors:
    - kind: github
      url: https://github.com/sponsors/octocat
    - kind: kofi
      url: https://ko-fi.com/octocat
    - kind: url
      url: https://opencollective.com/octocat/donate
  ```
  Locked kind whitelist: `github` | `polar` | `kofi` | `url`. Each kind
  has an anchored per-host regex (CLI first line of defence; registry
  re-validates server-side — defense in depth). The `url` escape hatch
  parses through `url::Url`, requires `https://`, non-empty host, and
  total length ≤ 256 chars.
  - **`pakx-core`** gains the `Sponsor` / `SponsorKind` types, the
    `validate_sponsors(&[Sponsor]) -> Result<(), SponsorError>` helper,
    and a per-kind `LazyLock<Regex>` so regexes compile once per
    process.
  - **`pakx pack`** YAML-parses the SKILL.md frontmatter (the previous
    v0.1 `name:` / `version:` line scanner was extended into a real
    `serde_yaml_ng`-backed parse) and trips with `sponsors[0].url:
    does not match the github URL shape: ...` on malformed entries,
    `sponsors: too many entries (6); max 5` on overflow, before any
    tarball bytes hit disk.
  - **`pakx publish`** emits a `sponsors` JSON array in the POST body
    to `/api/v1/packages` when the manifest declares any. The field
    is **omitted** (not `null`, not `[]`) when the manifest has no
    `sponsors:` block — the registry treats absent as "no change" but
    an explicit `[]` as "clear", so omitting on empty avoids wiping
    sponsors on a republish from an older manifest.
  - **`pakx info`** decodes a `sponsors[]` field on the GET response
    and renders it as a `sponsors:` block between the description /
    `registry:` line and the versions table on the human surface (spec
    §7 open-question #7 ordering). The `--json` contract surfaces a
    stable `sponsors` field (always an array, empty when none) so
    downstream `jq` consumers never need to null-check.
  - **`pakx-registry-client`** continues to ride the `extra` flatten
    capture on `DetailResponse`, so sponsors flow through
    `Package.install_hints["sponsors"]` for downstream consumers (the
    Phase 2c `pakx-web` package-detail page is the next user).

### Tests

- **Regression coverage for federated `pakx search --json` surfacing
  both pakx-registry and Smithery hits.** The 2026-05 incident report
  flagged that `pakx search hello-world --json` against production
  returned 10 Smithery hits and zero pakx-registry hits even though
  `arwenizEr/hello-world@0.1.1` was live. Root cause turned out to
  be upstream of the CLI — the registry list endpoint's
  `latestVersion` subquery was returning `null`. With that fixed
  server-side, the CLI now surfaces pakx-registry hits correctly,
  but the federated-merge contract was previously only covered by
  single-source unit tests. Two new tests pin it:
  - `pakx-registry-client/tests/pakx_source.rs::search_surfaces_prod_list_shape_with_latest_version`
    — wiremock-backed unit test asserting the prod list-endpoint
    shape (`id`, `kind`, `description`, `visibility`,
    `latestVersion`) decodes into a `Package` with the
    registry-supplied version, not the `"0.0.0"` fallback. The
    `visibility` field rides through `install_hints` via the
    flatten capture.
  - `pakx/tests/e2e_real_binary.rs::e2e_search_json_surfaces_pakx_registry_and_smithery`
    — `#[ignore]`d real-binary e2e mocking `OfficialMcp` empty,
    Smithery with one hit, and pakx-registry with one hit, then
    asserting `pakx search hello-world --json` contains **both**
    `source: "smithery"` and `source: "pakx"` entries with the
    correct ids and version.

### Fixed

- **Action subcommands no longer leak absolute host paths into error
  messages.** Every CI log embedding a `pakx test` / `pakx install` /
  `pakx add` / `pakx remove` / `pakx init` / `pakx pack` / `pakx
  publish` error previously contained the runner workspace path
  (e.g. `C:\Users\runneradmin\AppData\Local\Temp\…` or
  `/home/runner/work/<org>/<repo>/…`) verbatim. On self-hosted
  runners this also leaks the operator's username. Error messages
  now render paths relative to the project root when the target lives
  underneath it, and fall back to the basename when it doesn't.
  Implemented in a shared `pakx::redact::redact_path` helper used by
  every action subcommand's `with_context` call sites, plus a
  matching redact step on `pakx-core`'s `ManifestError` /
  `LockfileError` / `Credentials` Display impls so the underlying
  cause chain stays redacted too. The post-action hint lines
  (`→ lockfile: <abs path>`) are intentionally **not** redacted —
  they go to stdout for user value, and the user's next action
  (`git add`) needs the absolute form.

- **`pakx pack` now accepts CRLF-encoded SKILL.md frontmatter.**
  Notepad and VSCode-on-Windows (default LF→CRLF auto-fix) save
  `SKILL.md` with `\r\n` line endings. The fence scanner previously
  matched only `\n` (`strip_prefix("---\n")` + `find("\n---")`), so a
  CRLF-saved file silently fell through and the YAML parser saw
  `name: demo\r` / `version: 0.1.0\r` as part of the markdown body
  instead of the frontmatter — surfacing as a confusing "missing
  `name:`" error. The frontmatter extractor now normalises CRLF→LF
  before fence detection.

- **`pakx test` now rejects `--no-smithery --smithery-base-url …` and
  `--no-pakx-registry --pakx-base-url …` combinations** with a clap
  conflict error. The previous round wired the `conflicts_with` guard
  on `pakx install` only — `pakx test` had the same flag pair but no
  guard, so the override URL was silently dropped when the matching
  `--no-*` flag was also passed. The two surfaces are now in
  lockstep.

- **`pakx install` against a published skill no longer fails with
  `registry response for <id>@<version> omits tarballUrl`.** The PR #36
  resolver wired the install step to `GET /api/v1/packages/{owner}/{name}`
  — the list/detail endpoint, which deliberately omits the signed
  `tarballUrl` (signed URLs are short-TTL; the backend doesn't mint one
  per `versions[]` entry). Live install against the first published
  package `arwenizEr/hello-world@0.1.1` therefore always failed. The
  resolver now calls `GET /api/v1/packages/{owner}/{name}/{version}`
  (per-version endpoint, see backend route
  `app/api/v1/packages/[owner]/[name]/[version]/route.ts:57-65`) which
  returns the fresh signed `tarballUrl` alongside the per-version
  `sha256`, `sizeBytes`, `publishedAt`, and `deprecatedAt`. Pinned
  deps skip the list call entirely and go straight to the per-version
  endpoint; unpinned deps still hit the list endpoint to enumerate
  `versions[]` and pick latest / highest-semver. The per-version
  response is **never cached** — signed URLs would expire while the
  cache TTL is still valid, breaking subsequent installs with a 403
  from blob storage.

### Added

- **`PakxSource::fetch_version(owner, name, version)` →
  `Result<PackageVersion, RegistryError>`** in `pakx-registry-client`.
  Wire-format `PackageVersion` mirrors the backend response: `id`,
  `version`, `sha256`, `sizeBytes`, `publishedAt`, `deprecatedAt`,
  `tarballUrl`, plus an `extra` flatten capture so additive backend
  fields don't break the CLI. Used by the install-skill resolver and
  exposed for downstream consumers (e.g. `pakx doctor` will use it to
  re-verify a lockfile's `resolvedFrom` against current registry
  state once that wiring lands).
- **Post-action next-step hints across every action subcommand.**
  One dimmed line trailing the success line, prefixed with `→`
  (U+2192), telling users what to run next or where to look:
  - `pakx add <id>` → `→ next: pakx install`
  - `pakx remove <id>` → `→ next: pakx install`
  - `pakx install` (on success) → `→ lockfile: <absolute path>`
  - `pakx test` (on success) → `→ manifest validated`
  - `pakx pack` → `→ next: pakx publish`
  - `pakx publish` → `→ view: https://pakx.dev/p/pakx/<owner>/<name>`
  - `pakx unpublish` → `→ deprecated <owner>/<name>@<version>: 30-day
    soft-delete grace; resolves to 404 after the window closes`
  - `pakx login` → `→ credentials: <path> (mode 0600)` (the
    `(mode 0600)` suffix is unix-only)
  Read-only subcommands (`list`, `search`, `info`, `whoami`, `config`,
  `doctor`, `upgrade`) deliberately stay hint-free. JSON output paths
  remain unaffected — the `--json` contract surface emits exactly
  the JSON line and nothing else.

### Security

- `pakx pack` now **refuses** to follow symlinks under the source
  tree. The previous walker used `file_type().is_file()`, which follows
  symlinks transparently — a hostile skill template could include a
  symlink to `~/.ssh/id_rsa` or `/etc/shadow` and the target's contents
  would be packed into the tarball that `pakx publish` then uploads.
  Symlinks now produce an explicit `symlinks under SKILL.md src/ are
  not allowed: <path>` error so the author sees the surprise before
  upload. Silently skipping was rejected as the wrong UX for a publish
  flow.
- `pakx test --mcp-base-url` / `--smithery-base-url` /
  `--pakx-base-url` userinfo bypass. The old `starts_with` +
  `split('/')` parser accepted `http://localhost:8080@evil.com/`
  because the substring before the path looked loopback-like, even
  though the real host is `evil.com`. The override is now validated
  via `url::Url`: only `https://` everywhere or `http://` against
  `localhost` / `127.0.0.1` / `[::1]` pass, and any URL carrying a
  username or password is rejected outright.
- `~/.pakx/credentials.json` is now created with mode `0600` at the
  `open` call (via `OpenOptions::mode`) and written atomically through
  a `.tmp` sibling that is `rename`d into place. The previous
  `std::fs::write` then `set_permissions` flow briefly exposed the
  token at the default umask between the two calls, readable by other
  local users on a multi-user host. The on-disk path now never exists
  at any mode other than `0600` (unix). Atomicity also means a crash
  mid-write no longer leaves a half-written file. The `.tmp` sibling
  is unlinked before each open so a stale `.tmp` left by a prior
  crash (or pre-planted by a co-process) cannot bypass the mode bits
  — `OpenOptions::mode(0o600)` is ignored on existing files, so the
  only safe path is to ensure the file is created fresh every time.
- `PakxBackend::upload_version` and `PakxBackend::unpublish` now
  percent-encode every path segment (`owner` / `name` / `version`)
  before building the URL, **and** validate `name` shape up-front
  (rejecting `..`, leading `.`, embedded `..`, `/`, `\`, control
  chars, or empty). Without this, a package name like `..` would
  still produce a URL with literal `..` segments (the encoder
  deliberately leaves `.` unreserved) that HTTP routers normalize
  away, silently routing the `PUT` / `DELETE` to a different
  endpoint. `PakxSource::fetch` already had the encoding; the
  publish side now matches and adds the shape guard.

### Fixed

- `pakx add` now accepts the two-positional `<kind> <id>` form
  (`pakx add mcp foo/bar`) alongside the existing `pakx add <id>
  [-t <kind>]` shape. Users naturally try the kind-first form because
  every other package manager works that way; previously clap
  rejected the extra positional with `error: unexpected argument
  'foo/bar'`. The two-positional form is mutually exclusive with
  `-t/--type` (errors with `kind specified twice`), and an invalid
  kind token in the first positional is rejected with a list of the
  valid kinds rather than being silently treated as the id.
- `LockfileError` now has a dedicated `Io` variant. The previous code
  wrapped every `std::io::Error` from `read_lockfile_from` /
  `write_lockfile_to` in `LockfileError::Schema { message: "io error:
  ..." }`, so a permission-denied or disk-full on `agents.lock`
  surfaced to the user as "failed schema validation" — misleading and
  hard to diagnose. IO errors now render as `"agents.lock io error
  at <path>: <reason>"`.
- `Credentials::Entry` is now `#[serde(deny_unknown_fields)]`. A typo
  in `credentials.json` (or a future-version field we don't model
  yet) surfaces as a parse error instead of being silently dropped on
  round-trip — losing the `token` field would be catastrophic, and
  this guards the contract.
- **`pakx install` and `pakx test` now actually resolve through all
  federated registries** (official MCP Registry + Smithery +
  pakx-registry), matching the README + CHANGELOG claims and the
  `--no-smithery` / `--no-pakx-registry` flag layout. The previous
  implementation only called `OfficialMcpSource::fetch`; if a dep
  wasn't in the MCP Registry, the resolver gave up — even though
  `pakx search` was already returning hits from Smithery and
  pakx-registry. The dead flags are now live. Resolution strategy:
  - Try `OfficialMcp.fetch` first (preserves the canonical-source
    pin for upstream MCP servers).
  - On `NotFound`, run `client.search(&id)` across the remaining
    registered sources (the `OfficialMcp` source is filtered out of
    this fallback fan-out because its hit was already discarded one
    line up — saves one round-trip per resolved dep) and pick the
    first exact-name match.
  - `agents.lock` now records **which source** resolved the dep
    (`registry: "smithery"`, `"pakx"`, etc.) so `pakx doctor`
    can reason about drift without re-running the federated search.
  - Test-only base-URL overrides (`--smithery-base-url`,
    `--pakx-base-url`) added to `pakx install` to match `pakx test`,
    **and** the same userinfo-bypass guard that protects `pakx test`
    (`validate_base_url`) is now applied to `pakx install` so the
    two surfaces stay in lockstep.
  - `pakx install`'s `--no-smithery` / `--no-pakx-registry` flags
    now conflict with their matching `--*-base-url` overrides —
    `--no-smithery --smithery-base-url …` is a contradiction and
    clap errors immediately instead of silently ignoring the URL.
- `OfficialMcpSource::fetch` search fallback now picks deterministically
  when the registry returns multiple entries with the same canonical
  name. Previously the first match in the result set won, so re-fetches
  could pin a `0.0.0` placeholder when a real version was also
  available. The picker now prefers entries with a non-placeholder
  version, tie-breaking on lexicographic version desc.

### Added

- **`pakx install` now resolves, downloads, verifies, and extracts
  `skills:` dependencies through pakx-registry.** Previously the
  install loop only handled `mcp:` deps; skills were silently
  classified as `not yet supported`, leaving every published skill
  uninstallable (no consumer flow existed for the first published
  package, `arwenizEr/hello-world@0.1.1`). The new path:
  - Resolves the manifest shorthand `<owner>/<name>[@<version>]`
    against `GET /api/v1/packages/{owner}/{name}` on pakx-registry.
    Pinned versions are honoured; unpinned deps fall back to the
    API's `latestVersion` hint, and when that returns `null` (the
    current behaviour per pakx-publish-smoke notes) to the highest
    non-deprecated semver in `versions[]`.
  - Streams the signed `tarballUrl` to a `tempfile::NamedTempFile`
    with a 50 MiB hard cap; abort + cleanup on overflow.
  - Sha256-verifies the downloaded bytes against the API-declared
    `sha256` before any extraction step. Mismatch errors with
    `integrity mismatch for <owner>/<name>: expected …, got …` and
    deletes the staging file.
  - Untars (over gzip) into `<claude-home>/skills/<owner>-<name>/`
    (matches Claude Code's organic skills layout). Four
    defense-in-depth guards fire on every entry: refuses absolute
    paths, refuses `..` components (zip-slip), refuses symlinks and
    hardlinks (defense in depth — `pakx pack` already refuses
    symlinks server-side), and caps the **decompressed** total at
    50 MiB to defeat a zip-bomb hiding behind cheap-to-stream
    compression.
  - Writes an `agents.lock` entry with `registry: "pakx"`,
    `integrity: "sha256-<base64>"`, and `resolved_from` set to the
    **canonical** pakx-registry URL (signed `?download=…` query
    stripped — the signature is ephemeral, the canonical path is
    the stable record).
  - When `--no-pakx-registry` is passed, skill installs fail
    cleanly with a "skill installs require pakx-registry; refused"
    message rather than silently dropping the dep.
  - Other adapters (cursor / codex / copilot / windsurf) do not
    yet implement skills extraction; the Claude Code path runs
    whenever a Claude home is configured (override or default),
    which is always the case under the current runner.
- `pakx remove <id>` — inverse of `pakx add`. Strips a single shorthand
  dep from `agents.yml` after a `[y/N]` confirmation (skip with `--yes`).
  Kind is inferred when the id is unambiguous; supplying `--kind
  <mcp|skills|subagents|prompts|commands|hooks>` is required when the
  same id is declared under multiple sections (the resolver errors with
  the list of conflicting sections instead of silently picking one).
  `--directory` mirrors `pakx install` / `pakx list`. Does **not**
  touch `agents.lock` or installed adapter state — matches the
  `pakx add` symmetry, so the next `pakx install` is the single point
  that reconciles both. Round-trips clean: `pakx add` followed by
  `pakx remove` on the same id returns the parsed manifest to its
  pre-`add` shape.
- `pakx list --json` and `pakx search --json` — single-line JSON array on
  stdout (newline-terminated) so output pipes cleanly into `jq`. Both
  share the same upstream data structure as the human-readable view —
  no second code path. Field names are a stable contract:
  - `list`: `key`, `id`, `version`, `type`, `registry`, `resolved_from`,
    `integrity`, `agents`, `status` (`ok` | `drift` | `unknown`).
  - `search`: `id`, `name`, `version`, `source`, `description`.
    `description` is **always present** in the JSON output (empty
    string when upstream has no description) so `jq '.description'`
    never returns `null` — the field shape is invariant across hits.
    `--no-pakx` is honoured.
- `pakx test` — read-only manifest validation for CI / pre-commit use.
  Parses `agents.yml` and (unless `--offline`) resolves every `mcp:`
  entry against the federated registries — official MCP Registry +
  Smithery + pakx-registry by default; toggle with `--no-smithery` and
  `--no-pakx-registry` (matches `pakx search`'s flag layout). Prints a
  per-entry `ok` / `fail: <reason>` line and exits non-zero on the
  first failure. **Scope-narrowed to `mcp:` deps for this version:**
  other dep kinds (`skills:` / `subagents:` / `prompts:` / `commands:`
  / `hooks:`) are reported as `skip (not yet validated: ...)` and a
  single `note: skipped N entries (only mcp: validated in this
  version)` line is written to stderr when any were skipped. They are
  NOT counted as failures — that would break the CI contract for
  manifests that already declare those dep kinds waiting for adapter
  wiring. Does not write `agents.lock`, does not touch the install
  dir. `--offline` checks deps against the existing lockfile only;
  `--manifest <path>` overrides the default `agents.yml` location.
  Hidden test-only base-URL overrides (`--mcp-base-url` /
  `--smithery-base-url` / `--pakx-base-url`) require `https://` or
  `http://localhost` / `http://127.0.0.1` — any other plaintext URL is
  rejected to prevent silent exfiltration of manifest contents in CI.

## [0.1.3] — 2026-05-21

### Fixed

- **`pakx install` against `io.github.bytedance/mcp-server-filesystem`
  (and every other 2025-12-11-schema MCP Registry entry) — was failing
  with `not found in official MCP registry` and `no installable
  transport advertised` because the schema renamed every field we
  decode from snake_case to camelCase and moved the transport hint
  inside each package.**
  - `OfficialMcpSource::fetch` now falls back from the per-server
    detail endpoint (which 404s on the current schema) to
    `?search=<id>` exact-name match. The old detail endpoint is still
    tried first so legacy deployments keep working.
  - `mcp_translate::PackageHint` accepts both `registry_name` /
    `registryType`, `name` / `identifier`, `package_arguments` /
    `packageArguments`, and `environment_variables` /
    `environmentVariables` via serde aliases.
  - `mcp_translate::pick_remote` now also walks `packages[].transport`
    so hosted SSE / streamable-http servers resolve to
    `McpTransport::Http` even when the response has no top-level
    `remotes[]` array.
  - Recognises `npm` / `npmjs` / `npmjs.org`, `pypi` / `pypi.org`,
    and `docker` / `oci` / `ghcr` / `ghcr.io` as stdio-launchable
    registry types.

End-to-end verified on Windows-GNU: `pakx install` against
`io.github.bytedance/mcp-server-filesystem` writes the right
`.mcp.json` with `npx -y @agent-infra/mcp-server-filesystem`, and
`pakx doctor` reports all checks passed.

## [0.1.2] — 2026-05-21

### Added

- `pakx upgrade` — checks `github.com/repos/pakxdev/pakx/releases/latest`
  and prints the channel-appropriate upgrade command (curl|sh,
  irm|iex, `brew upgrade pakx`, `scoop update pakx`, or `cargo
  install --tag`). Read-only by design — does not rewrite the
  currently-installed binary because that path varies per channel
  and trying to be clever ruins installs.
- Governance: `.github/ISSUE_TEMPLATE/*`, `.github/PULL_REQUEST_TEMPLATE.md`,
  `.github/FUNDING.yml`. Issue templates use structured forms with
  version + platform + reproducer fields; PR template defaults the
  test plan to the local `fmt`/`clippy`/`test` commands.
- `examples/hello-world/SKILL.md` — minimal publishable skill, walks
  through `pakx login`/`pakx pack`/`pakx publish`.

### Distribution

- Homebrew tap shipped: `brew install pakxdev/tap/pakx`.
- Scoop bucket shipped: `scoop bucket add pakx ... && scoop install pakx`.
- CycloneDX 1.3 SBOM (`pakx-v0.1.1-sbom.cdx.json`) attached to the
  v0.1.1 GitHub Release for downstream vulnerability scanners.

## [0.1.1] — 2026-05-21

Security + portability cleanup. No CLI behaviour change.

### Changed

- **`reqwest` switched from `default-tls` to `rustls-tls`.** Drops the
  OpenSSL runtime dependency, removes one large attack surface, and
  makes cross-compilation portable (no system OpenSSL needed for
  Linux / macOS / Windows builds).
- **`serde_yml` → `serde_yaml_ng`.** Resolves
  [RUSTSEC-2025-0067](https://rustsec.org/advisories/RUSTSEC-2025-0067)
  (`libyml::string::yaml_string_extend` unsound) and
  [RUSTSEC-2025-0068](https://rustsec.org/advisories/RUSTSEC-2025-0068)
  (`serde_yml` unmaintained). `serde_yaml_ng` is the actively
  maintained drop-in fork of the original `serde_yaml`.
- **`inquire` 0.7 → 0.9.** Drops the transitive dependency on
  unmaintained `fxhash` ([RUSTSEC-2025-0057](https://rustsec.org/advisories/RUSTSEC-2025-0057)).

### Added

- Cross-platform prebuilt binaries: `aarch64-apple-darwin`,
  `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, and `x86_64-pc-windows-gnu`. Built
  locally with `cargo zigbuild` because GitHub Actions is
  temporarily disabled to control CI billing.
- Governance docs: `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`.

### Verified

- `cargo audit` — 0 vulnerabilities, 0 warnings across 288 deps.
- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace --no-fail-fast` — 200+ tests, 0 failures.

## [0.1.0] — 2026-05-21

First public early-access release. CLI feature-complete for the manifest → resolve → install → publish loop against the live [registry.pakx.dev](https://registry.pakx.dev).

### Added

- `pakx init` — interactive `agents.yml` scaffolder.
- `pakx add <id>` — append a dependency to the manifest with best-effort registry validation.
- `pakx install` — resolve every MCP dependency via the federated registry, write `agents.lock`, and project-install into Claude Code's `.mcp.json`.
- `pakx list` — pinned lockfile entries with `[ok]` / `[drift]` markers.
- `pakx doctor` — five-section health check (manifest, lockfile, drift, adapter detection, on-disk vs lockfile).
- `pakx search <query>` — federated free-text search across the official MCP Registry, Smithery, and the first-party pakx-registry.
- `pakx login` — GitHub-backed login that validates an API token against `/api/v1/whoami` and stores it in `~/.pakx/credentials.json` (mode `0600`).
- `pakx whoami` — stored login or live whoami; `--offline` skips the network round-trip.
- `pakx pack` — deterministic gzipped tarball builder (sorted entries, zeroed mtime/uid/gid, mode `0o644`, ≤ 50 MiB).
- `pakx publish` — `pack` → `POST` package → `PUT` tarball. `--dry-run` skips the upload.
- `pakx unpublish <owner>/<name>@<version>` — `DELETE` with grace-period tombstoning on the server side.
- `PakxSource` federated source — `GET /api/v1/packages` for search, `GET /api/v1/packages/{owner}/{name}` for detail. Opt out with `--no-pakx`.
- Per-agent adapters for Claude Code (reference), Cursor, Codex, Copilot, Windsurf (detect-only at v0.1).

### Fixed

- Official MCP Registry schema drift: list and detail responses now wrap each entry in `{ server, _meta }` (2025-12-11 schema). `ServerRaw` accepts both the wrapped shape and the legacy flat shape.

### Known limitations

- No prebuilt binaries yet — install via `cargo install --git` or the build-from-source `install.sh` / `install.ps1` on https://pakx.dev. Prebuilt binaries (Homebrew tap, Scoop bucket, Winget manifest) ship at v0.2.
- Smithery is search-only; install translation is on the v0.2 roadmap.
- GitHub Actions CI is temporarily disabled to control billing; all verification is local-first until the release pipeline lands.

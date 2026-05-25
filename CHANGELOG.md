# Changelog

All notable changes to pakx will be documented in this file.

The format roughly follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and pakx follows [Semantic Versioning](https://semver.org).

## [Unreleased]

## [0.1.9] — 2026-05-25

### Fixed

- **`pakx list` and other tables now align correctly when color is
  enabled.** With a color terminal (or `--color always`), colored cells —
  such as the green `[ok]` badge in `pakx list` — carry ANSI escape bytes
  that the table renderer was counting toward column width. The escape
  bytes inflated the measured width, so the box-drawing borders no longer
  lined up. The table renderer is now ANSI-aware (comfy-table
  `custom_styling`): columns are measured by their visible width, so
  bordered tables align correctly whether or not color is on.

## [0.1.8] — 2026-05-25

### Fixed

- **`pakx publish` no longer panics on certain registry responses.** After
  a successful upload, `publish` byte-sliced the registry-returned sha256
  to display a short digest. On a response whose digest was shorter than
  expected — or contained a multibyte character — the slice landed on a
  non-char boundary and panicked, turning a successful publish into a
  crash with no actionable output. The digest is now shortened on
  character boundaries (falling back to the full string when it is shorter
  than the display width), so the post-upload summary renders cleanly
  regardless of the exact registry response shape.
- **`pakx add` and `pakx install` are now honest about what they did.**
  Interactive runs no longer double-render their output when attached to a
  TTY. `pakx add` warns and suppresses the usual "next step" hint when an
  MCP entry has no installable transport (there is nothing to install, so
  it no longer implies otherwise). Install failures now carry actionable
  error messages instead of bare backend codes, and the `.mcp.json`
  rollback path no longer clobbers an existing file. Dependencies for
  not-yet-supported kinds are routed to *skipped* rather than reported as
  *failed*, so a partially-supported manifest no longer reads as broken.
- **UX and correctness hardening across the whole command surface.** Stream
  discipline was tightened on `pakx update` and `pakx outdated` (status
  noise no longer interleaves with machine-readable output). `pakx search`
  now distinguishes a degraded registry response from a genuinely empty
  result set. `pakx unpublish` gained a confirmation gate. `pakx manifest`
  edits validate before writing so a rejected change can no longer leave a
  half-written manifest on disk. Roughly fifteen further error and hint
  messages across the command set were rewritten to be actionable rather
  than terse.

## [0.1.7] — 2026-05-25

### Added

- **`pakx upgrade` now detects how the binary was installed and auto-runs
  the matching upgrade command.** Previously `pakx upgrade` printed a
  generic hint and left the actual upgrade to the user. It now inspects
  `current_exe()` to identify the install channel — Cargo
  (`cargo install`), the install script (`install.sh` / `install.ps1`),
  Homebrew, or Scoop — and runs the channel-appropriate command for you
  (`cargo install ... --force`, re-run of the install script, `brew
  upgrade`, `scoop update`). The action is confirm-gated; pass `--yes` to
  skip the prompt, or `--check` for a read-only report of the detected
  channel and what would run. The prompt is non-TTY-safe (fails fast with
  a clear message instead of hanging when there is no controlling
  terminal). On Windows the script channel prints the command to run in a
  fresh shell rather than attempting an in-place self-replace.

## [0.1.6] — 2026-05-25

### Fixed

- **Interactive confirmation prompts now fail fast instead of hanging
  when stdin is not a TTY.** `pakx remove` (and the interactive prompts
  in `update` / `init` / `new`) previously blocked forever waiting on a
  confirmation that could never arrive when run in CI or a piped script
  with no controlling terminal. They now detect the missing TTY up
  front and exit with a clear error pointing at the `--yes` flag instead
  of hanging.
- Isolated the per-call-cache cleanup tests from a shared tempdir so
  they no longer flake on `macos-latest` CI. Test-only change; no
  user-facing behaviour difference.

## [0.1.5] — 2026-05-25

### Added

- **`pakx install --rollback-on-error` — opt-in all-or-nothing
  install.** When any dependency fails partway through a multi-dep
  install, the default behaviour leaves a half-installed tree across
  `~/.claude/{skills,agents,commands,prompts,hooks}` and the
  project-scoped `.mcp.json` merge target. With `--rollback-on-error`,
  the runner snapshots the prior on-disk state of every target the run
  will touch (derived purely from the manifest's declared deps) before
  the first adapter write, and on a failed run restores local state to
  exactly what it was before: dirs the run created are removed, dirs
  that pre-existed are moved back with their prior contents (via
  rename, atomic within a filesystem). The post-rollback report is
  re-cast so the summary and `--json` payload don't claim installs that
  were reverted. Opt-in only this version; default-on is reserved for a
  future major bump. Without the flag, the prior partial-install
  behaviour is preserved exactly.

- **Per-dependency `pakx install` progress.** The single whole-run
  spinner is replaced by one `indicatif::MultiProgress` bar per
  dependency. Each bar advances through its lifecycle (resolve →
  install) and settles to a terminal `[ok]` / `[skip]` / `[fail]`
  line, so cold-cache installs of several deps show per-dep progress
  instead of one opaque spinner. A presentation-only `ProgressSink`
  seam keeps the runner logic unchanged; non-TTY / `--json` paths use a
  no-op sink and render nothing extra on stderr.

- **`pakx audit --offline` — lockfile-only audit, no network.** The
  deprecation signal lives behind a live `fetch_version` request that
  intentionally bypasses the cache (signed-URL TTL discipline), so an
  offline audit cannot know whether a pakx entry is deprecated. Rather
  than emit a misleading `ok`, every pakx entry is reported as `skip`
  with a `not checked (offline)` note — exit-code-neutral, but
  distinguished from the structural "no deprecation signal" skip so a
  consumer can tell "unknown offline" apart. Lets the audit run in
  airgapped / no-egress CI without any network I/O.

- **`pakx new <kind> <name>` (alias `scaffold`) — generate a
  publishable bundle.** `pakx init` writes a *consumer* `agents.yml`;
  there was previously no command to scaffold a *publisher* bundle.
  `pakx new <kind> <name>` generates a correct starter tree in one
  command for five kinds — `skills`, `subagents`, `prompts`,
  `commands`, `hooks` — each producing a bundle that passes the
  per-kind `pakx pack` validation (below) with zero warnings:
  - `skills` → `SKILL.md` with `description:` frontmatter.
  - `subagents` → `SKILL.md` with a kebab-case `name:` + `description:`.
  - `commands` → `SKILL.md` with `description:`.
  - `prompts` → a non-empty `prompt.md` alongside the manifest.
  - `hooks` → a `hooks.json` declaring a `PreToolUse` event + matcher.

  `mcp` is rejected with a pointer to `pakx add mcp <id>` — an MCP
  server is registry config, not a packable file bundle, so there is
  nothing honest to scaffold.

- **`pakx search --kind <kind>` — filter search results by kind.**
  `pakx search` was the last kind-blind command. It now forwards
  `?kind=<kind>` to the pakx-registry's existing list endpoint
  (server-side filter, composed with `?q=`) and filters federated hits
  client-side. Federated sources (Smithery, official MCP Registry) have
  no kind concept and match only `--kind mcp`. The kind token is parsed
  via the same canonical plural token set as `pakx add <kind> <id>`,
  and a tolerant normalizer folds the registry's singular `"skill"`
  onto the CLI's plural `"skills"`. Search output gains a `kind` column
  on the human table and an additive `kind` field on each `--json` hit
  (null for federated sources). This completes "kind first-class
  everywhere" alongside the `pakx list` kind column, `pakx tree`,
  `pakx info` kind, and `pakx new <kind>`.

- **`pakx list` kind column.** The pinned-lockfile table now surfaces
  each entry's kind as a dedicated column, making the manifest section
  a dep was declared under visible at a glance.

- **`pakx info <id> <field>` — npm-view-style field query.** An
  optional positional `<field>` arg accepts the same dot/bracket path
  syntax `pakx manifest get` uses (e.g. `versions[0].sha256`,
  `description`, `sponsors[0].url`). When set, the command prints only
  that field. Output discipline mirrors `npm view <pkg> <field>`:
  scalar strings print unquoted, numbers / bools / null route through
  `Display`, arrays / objects emit as compact single-line JSON.

- **`pakx pack` reads `README.md` and `pakx publish` forwards it to the
  registry.** Publishers can ship a long-form `README.md` alongside
  their bundle; `pack` reads `<src>/README.md` (UTF-8 lossy decode),
  truncating to a UTF-8 char boundary with a non-fatal warning past the
  256 KiB registry cap. `publish` forwards it: the `readme` field in
  the POST body plus an `x-pakx-readme-b64` header on the tarball PUT
  (keeps the PUT body on `application/gzip` for the registry's
  Content-Length pre-check). Absent README → the field is omitted
  entirely, so a republish from a README-less bundle never wipes a
  previously-stored README. The pakx-web `/p/*` detail page renders the
  stored markdown.

- **`pakx export <id>` — copy an installed package's on-disk tree into
  a portable folder.** Resolves the id via the lockfile (no network
  round-trip), copies the tree under `<claude_home>/<subdir>/<owner>-<name>/`
  into `<cwd>/<name-after-slash>` (or `--output <DIR>`). Refuses to
  overwrite an existing directory unless `--force`. `--json` emits
  `{from, to, files}`. MCP entries are rejected because their install
  state lives in `.mcp.json`, not in a per-package tree.

- **`pakx pack --output <DIR>`** as the canonical long form for
  selecting the tarball output directory. The historical `--out` alias
  remains for one release.

- **`pakx pack --dry-run [--json]`** enumerates the tarball entries
  without writing the `.tgz`. The `--json` payload extends the regular
  pack contract with `dryRun: true` and `files: [{path, sizeBytes}]`.

- **`pakx install --json`** emits `[{id, status, kind, version, error?}]`
  on stdout. Human progress + summary still render on stderr. Mirrors
  the `pakx outdated --json` shape.

- **`--no-cache` global flag** on `pakx search`, `pakx info`,
  `pakx outdated`, `pakx audit`, `pakx add`, and `pakx install`. Drops
  the per-call federated-source cache TTL to zero so the registry is
  re-queried rather than serving a stale response. Useful right after
  a publish.

- **`pakx doctor --clear-cache`** wipes every per-call cache directory
  pakx may have left under `std::env::temp_dir()` (including the
  persistent `pakx-install-cache` root). Best-effort; surfaces
  per-entry removal failures on stderr without tripping the doctor
  exit code.

### Fixed

- **`pakx search --limit` clamped to >= 1.** `-n 0` previously
  returned an empty list silently; clap now rejects the value at parse
  time with a clean diagnostic.

- **`pakx update` validates `--*-base-url` BEFORE the outdated
  probe.** Previously the validation fired only after `gather_outdated`
  emitted per-entry stderr noise, hiding the rejection. Validation
  now runs at the top of `run` so a userinfo-smuggled override surfaces
  as the only error.

- **`pakx outdated --help` documents the exit-code contract for the
  no-lockfile case.** The `--help` output now spells out that an
  absent lockfile exits 0 (no drift can exist without pins), matching
  the existing behaviour.

### Changed

- **`pakx pack` per-kind validation warnings (skills / subagents /
  commands / prompts / hooks).** Round 35 relaxed per-kind validation
  on the premise that Claude Code lacked a public spec for the
  non-skill kinds; that premise is now stale — Claude Code publicly
  specs every kind. The round-32 skills-only `description:` warning is
  extended to the other kinds, driven by the declared `kind:`
  frontmatter (defaulting to `skills`). All checks append to the
  existing `PackOutput.warnings` vec and **never hard-error**, so
  local-smoke / air-gapped publishes still succeed (exit 0):
  - `skills` — warn if `SKILL.md` lacks a non-empty `description:`.
  - `subagents` — warn if no bundle markdown has frontmatter with both
    a kebab-case `name:` and a `description:`.
  - `commands` — warn if no bundle markdown declares a `description:`.
  - `prompts` — warn if the bundle ships no non-empty file besides the
    manifest.
  - `hooks` — warn if no `hooks.json` (or equivalent) is present.

  The relevant Claude Code doc URL is cited inline in each warning.

- **`pakx unpublish` no longer claims a 30-day soft-delete grace.** The
  prior copy ("30-day soft-delete grace; resolves to 404 after the
  window closes") was aspirational — no hard-delete cron exists on the
  pakx-registry backend. The corrected language: "still resolvable for
  existing pins but hidden from list endpoints." Existing pinned
  installs continue to work after `pakx unpublish` forever.

## [0.1.4] — 2026-05-23

### Changed

- **`pakx search --no-pakx` renamed to `--no-pakx-registry`** so the
  flag matches the pre-existing `--no-pakx-registry` on `pakx install`
  and `pakx test`. The three subcommands now share one flag name for
  the same source toggle. `--no-pakx` is retained as a hidden alias
  for one release; scripts continue to work without modification
  during the migration window.

- **`pakx update` is now its own subcommand.** It previously existed
  only as an alias for `pakx upgrade` (which upgrades the CLI binary
  itself). The alias is removed; users who typed `pakx update`
  expecting to upgrade the CLI must now type `pakx upgrade`. See the
  **Added** section for the new `pakx update` semantics (rewrite
  package pins in `agents.yml`).

- **`pakx login` defaults to the device authorization grant flow.**
  Previously, running `pakx login` with no `--token` argument dropped
  into an interactive token-paste prompt. It now runs the new
  `--device` flow against `POST /api/v1/auth/device` and prints a
  user-code + verification URL for browser confirmation. The legacy
  paste path is still reachable via `--token <pakx_v1_…>` or the
  `PAKX_TOKEN` environment variable (the env-var path is what CI
  runners should use). The interactive prompt is gone; scripts that
  relied on stdin-fed `pakx login` must move to `--token` or
  `PAKX_TOKEN`. The `--device` and `--token` flags are mutually
  exclusive.

### Deprecated

- **`--no-pakx` on `pakx search`** — use `--no-pakx-registry`. The
  alias will be removed in v0.2.

### Added

- **`pakx login --device` — device authorization grant.** New default
  login flow. `pakx login` (with no flags) prints a verification URL
  and a short user-code, opens the URL in the system browser when
  possible, and polls `POST /api/v1/auth/device/poll` until the
  registry returns `success`, `denied`, or `expired`. RFC-8628-style
  `slow_down` responses bump the poll interval by at least 5 seconds;
  the overall window matches the server-supplied `expires_in` (600s).
  Polling uses a monotonic `Instant`-based deadline so an NTP slew on
  `SystemTime` cannot collapse the loop or extend it past the
  registry's window. The token is never printed and never logged at
  `tracing` levels above `debug` — it goes from the HTTP response
  directly into the credentials file.

- **`pakx whoami --json` — machine-readable identity payload.** Emits
  a single newline-terminated JSON object matching the `pakx list
  --json` / `pakx info --json` style so pipelines can `jq` the
  result. Shape: `{login, id, email, registry, source}` where
  `source` is `"online"` (live whoami call succeeded), `"cached"`
  (the `--offline` short-circuit, or a transient network failure
  silently degraded the call — the cached entry never persisted
  `id` / `email`, both fields are `null`), or `"none"` (no stored
  entry for the targeted registry — `login` / `id` / `email` are
  `null`). Exit code is `0` when logged in (online or cached) and
  `1` when there is no stored entry, so a script can branch on the
  exit code without parsing JSON. The human (non-`--json`) path is
  unchanged — interactive users still see the coloured login line or
  the verbatim network error.

- **`pakx info <owner>/<name> --version <ver>` — per-version metadata
  block.** Fetches the immutable per-version endpoint
  (`GET /api/v1/packages/{owner}/{name}/{version}`) and renders the
  sha256, gzipped tarball size, publish timestamp (with a relative
  "N days ago" hint), and the **signed, short-TTL** download URL the
  installer uses. The human render closes with a `→ install:` hint
  showing the exact `pakx add <id>@<version>` invocation, and a
  footer note that the tarball URL expires after one hour (the
  registry's signed-URL TTL). `--json` emits the per-version API
  shape verbatim (`id`, `version`, `sha256`, `sizeBytes`,
  `publishedAt`, `deprecatedAt`, `tarballUrl`) for piping into `jq`.
  Without `--version`, the existing package-level metadata + version
  list render is unchanged. The `--version` form is pakx-source only
  today — federated MCP / Smithery sources don't expose a
  per-version block; the constraint matches the federated-info JSON
  path added in the previous round.

- **`pakx update` — rewrite `agents.yml` pins to a newer version,
  then reconcile via `pakx install`.** Closes the loop opened by
  `pakx outdated` (which only reports drift). Three input shapes:
  - `pakx update` (no args) — interactive prompt per outdated dep.
    `--yes` accepts every prompt without asking.
  - `pakx update <id>` — query the registry for the latest non-
    deprecated version of the matching dep and rewrite to that.
    Acts as if `--yes` was supplied (explicit invocation = consent).
  - `pakx update <id>@<version>` — pin verbatim, no registry round-
    trip. Allows downgrades and works even when the registry is
    unreachable.

  Flags: `--yes` / `-y` (CI-friendly accept-all), `--dry-run`
  (preview without writing), `--no-install` (rewrite manifest only,
  skip the auto-`pakx install`), `--directory <path>` (workspace
  override). The post-update install runs in-process — never spawns
  a child — so failures map to a single exit-code surface.

  Exit codes: `0` on success / nothing to do, `1` when the post-
  update install reconciliation fails, `2` when the registry cannot
  determine a target version (single-id form with no determinable
  newer version because every candidate registry erred).

  Alias `pakx up` for muscle memory. Note the (mismatched-but-
  intentional) naming: `pakx upgrade` continues to upgrade the
  **CLI binary itself** — `pakx update` updates **packages** in
  `agents.yml`. The README distinguishes them in the command table.

  Git and registry-object specs are out of scope at v0.1 and surface
  a hard error pointing the user at the shorthand-string form.

- **`pakx outdated` — show lockfile entries whose source registry has
  a newer non-deprecated version.** Reads `agents.lock` (canonical pin
  source) and queries each entry's recorded `registry` source:
  - `pakx` entries → `GET /api/v1/packages/{owner}/{name}` on
    `registry.pakx.dev`. Latest = first non-deprecated entry in the
    server-sorted `versions[]` array.
  - `official-mcp` entries → `OfficialMcpSource::fetch` and pick the
    `version` field.
  - `smithery` entries → `SmitherySource::fetch` similarly. Smithery's
    `"latest"` placeholder is surfaced as `status: unknown` because
    semver comparison is meaningless against a non-version literal.
  - `glama` / `github` / `git` entries are reported as `status: skip`
    until their resolvers land.

  Comparison is `semver`-aware: `latest > current` → `upgrade`,
  `latest < current` → `drift` (downgrade — usually means the pinned
  version was unpublished and rolled back), equal → up-to-date and
  excluded from the table. Registry unreachable → `status: error` on
  the row plus a `[warn]` line to stderr; the command does **not**
  fail (a transient network blip shouldn't break a CI gate that only
  cares about real drift). Exit code is `1` when anything is outdated
  (CI-friendly: `pakx outdated || echo "deps drift"`), `0` otherwise.
  Flags:
  - `--json` — single-line JSON array on stdout with stable field
    names (`id`, `current`, `latest`, `registry`, `status`, plus an
    optional `error` field on error rows). Up-to-date entries are
    excluded so `jq 'length'` produces the outdated count directly.
  - `--registry <pakx|official-mcp|smithery>` — restrict the check
    to one source. Useful in CI when only first-party drift matters.
  - `--directory <path>` — override the project root (mirrors `pakx
    list` / `pakx install`).
  - Hidden test-only overrides `--pakx-base-url` / `--mcp-base-url` /
    `--smithery-base-url`, validated against the same userinfo-
    smuggling guard `pakx install` and `pakx test` already enforce.

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

- **`pakx doctor --json` — machine-readable health report.** Emits a
  single newline-terminated JSON object with `ok`, `checks`,
  `warnings`, and `errors`. `checks` is a BTreeMap so wire order is
  deterministic. Errors flip `ok: false` and exit 1; warnings stay
  `ok: true` and exit 0. Closes the `--json` contract across every
  read-only subcommand.

- **`pakx pack --json` and `pakx publish --json`.** Single JSON line
  per invocation with `name`, `version`, `kind`, `sha256`,
  `sizeBytes`, `tarballPath`, `warnings` (pack) plus `registryUrl`,
  `tarballUrl`, `publishedAt` (publish). All progress + warnings
  stay on stderr so consumers can `pakx publish --json | jq
  .tarballUrl` end-to-end. `--dry-run` adds `dryRun: true` and omits
  the post-upload fields.

- **`pakx pack` warns on missing SKILL.md `description:`.** Non-fatal
  — the pack still produces a tarball, but the warning surfaces
  before publish so registry detail pages aren't shipped with empty
  descriptions. Claude Code uses the `description:` field to decide
  when to load a skill, so an empty one ships dead-on-arrival.

- **`pakx audit` — flag deprecated lockfile entries.** Reads
  `agents.lock`, queries the per-version registry endpoint for each
  pakx entry, and surfaces any with non-null `deprecatedAt`.
  Federated MCP / Smithery / glama / github / git entries are
  reported as `status: skip` (no deprecation signal). `--json` emits
  `[{id, version, registry, status, deprecatedAt, error?}]`. Exit 1
  on any deprecated entry, 0 otherwise.

- **`pakx tree` and `pakx why <id>`.** Tree pivots flat lockfile data
  into a grouped (kind, registry) tree with wired/skipped adapter
  status per row. Why does reverse lookup over `agents.yml` +
  `agents.lock` for a single id; multi-kind matches render every
  hit; `--kind <type>` filters. Both ship `--json` with stable
  shapes — `{kinds: {<kind>: {<registry>: [...]}}}` for tree and
  `[{id, kind, manifestSource, lockedVersion, registry, ...}]` for
  why. Exit code 0 in `--json` mode with `[]` on miss so jq
  pipelines don't break.

- **`pakx manifest get | set | delete <dot.path>`.** Scriptable
  read/write over `agents.yml` with `[N]` index syntax matching `npm
  pkg`. Built on a new `pakx_core::manifest::path` module that walks
  raw `serde_yaml_ng::Value` (separate from the typed
  `manifest::mutate` used by `pakx update`'s shorthand rewrites).
  `--json` mode emits/accepts JSON for non-string values. Atomic
  write via the existing `pakx_core::atomic_write` helper. Idempotent
  delete. Comment preservation deferred to a later release.

- **`pakx update --kind <type>` and `pakx remove --kind <type>`.**
  New flag on both subcommands disambiguates when the same id
  appears under multiple sections of `agents.yml` (e.g. `mcp:` and
  `skills:`). Without `--kind`, the prior auto-pick behaviour is
  preserved; with `--kind`, only entries of that kind are
  considered. Resolves the in-tree TODO at `update.rs:483-486`
  that promised this flag in a comment.

- **Sub-adapter installs for `commands`, `subagents`, `prompts`,
  `hooks`.** Previously `pakx install` silently `skipped` these
  kinds with a comment; declared deps under those sections of
  `agents.yml` were inert. New generic bundle installer in
  `crates/pakx/src/install/bundle.rs` routes each kind to its
  Claude Code directory: commands → `~/.claude/commands/<id>/`,
  subagents → `~/.claude/agents/<id>/`, prompts →
  `~/.claude/prompts/<id>/`, hooks → `~/.claude/hooks/<id>/`. The
  `--project` flag mirrors each into the project-local
  `.claude/<kind>/<id>/`. Per-kind validation is intentionally
  relaxed at v0 — no public AGENT.md or prompt frontmatter spec yet
  — so the installer accepts any well-formed tarball and logs an
  info-level "validation is best-effort" line. `pakx tree` and
  `pakx why` now report `adapter: wired` for all six kinds.

### Tests

- **Regression coverage for federated `pakx search --json` surfacing
  both pakx-registry and Smithery hits.** The 2026-05 incident report
  flagged that `pakx search hello-world --json` against production
  returned 10 Smithery hits and zero pakx-registry hits even though
  a known pakx-registry package was live. Root cause turned out to
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

- **Atomic, crash-safe writes for `agents.lock`, `agents.yml`, and
  every cached federated-registry response.** Each writer previously
  used `std::fs::write(path, body)` directly — a process crash or
  power-loss in the window between `open` and the final byte left a
  corrupt file on disk. For the federated cache that meant the
  next-fetch self-heals at the cost of one wasted network round-trip;
  for `agents.lock` it meant the next `pakx install` / `pakx test`
  failed hard against a half-pinned lockfile and the user had to `rm
  agents.lock` by hand to recover. The fix introduces a shared
  `pakx_core::atomic_write(path, bytes)` helper that writes to
  `<path>.tmp` and renames into place — POSIX `rename(2)` is atomic
  within a filesystem, and on Windows `std::fs::rename` lowers to
  `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, which is also atomic for
  files. Either platform: the destination is either untouched or
  fully written, never half. The orphan `.tmp` is unlinked
  best-effort on rename failure so failed runs don't leak temp files.
  The async `pakx-registry-client` cache reproduces the same
  temp-then-rename shape inline against `tokio::fs` (it can't call
  the sync helper from async code without `spawn_blocking`).
  The `~/.pakx/credentials.json` writer was already atomic from the
  PR #29 round and remains so — its `OpenOptions::mode(0o600)` flow
  is preserved because the new helper deliberately doesn't set
  permission bits (the lockfile + manifest + cache all want the
  default umask, and mode-at-open is the only atomic way to land
  sensitive bits on disk).

- **`pakx install` no longer overwrites `agents.lock` when any dep
  failed to install.** The previous flow wrote the lockfile
  unconditionally even when `report.failed` was non-empty — leaving
  a half-pinned lockfile on disk alongside a non-zero exit code.
  Downstream tools (`pakx test`, `pakx list`, `pakx doctor`) then
  saw an incomplete state that conflicted with the manifest's
  declared deps, and the user had to manually `rm agents.lock` to
  retry from a clean slate. The runner now gates the lockfile write
  on `report.failed.is_empty()`: a failed install leaves the prior
  `agents.lock` intact (or absent on a first install). The summary
  line still emits `installed N, skipped M, failed K` so the user
  sees exactly what happened. `--no-lockfile` continues to skip the
  write regardless, mirroring v0.1 behaviour. Regression test:
  `crates/pakx/tests/end_to_end.rs::install_failure_does_not_overwrite_existing_lockfile`.

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
  package therefore always failed. The resolver now calls
  `GET /api/v1/packages/{owner}/{name}/{version}` (per-version
  endpoint) which returns the fresh signed `tarballUrl` alongside the per-version
  `sha256`, `sizeBytes`, `publishedAt`, and `deprecatedAt`. Pinned
  deps skip the list call entirely and go straight to the per-version
  endpoint; unpinned deps still hit the list endpoint to enumerate
  `versions[]` and pick latest / highest-semver. The per-version
  response is **never cached** — signed URLs would expire while the
  cache TTL is still valid, breaking subsequent installs with a 403
  from blob storage.

- **Federated-source cache isolation in `outdated` / `search` /
  `add`.** Previously every invocation shared
  `std::env::temp_dir().join("pakx-<cmd>-cache")`. On Linux runners
  with aggressive port reuse, two sequential integration tests could
  share a cache entry seeded by an earlier test's wiremock server,
  surfacing as flaky CI failures. Cache root now carries `pid` plus
  `SystemTime` nanos so two invocations cannot collide.

- **Rust 1.95 `clippy::similar_names` compat.** Renamed `tmp` →
  `tmp_path` in `crates/pakx-core/tests/credentials.rs` so it no
  longer collides with the nearby `temp: TempDir`. Tightened lint
  was tripping the `-D warnings` ratchet against the existing pair.
  Test semantics unchanged.

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
  package). The new path:
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

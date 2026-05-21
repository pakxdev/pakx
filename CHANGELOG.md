# Changelog

All notable changes to pakx will be documented in this file.

The format roughly follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and pakx follows [Semantic Versioning](https://semver.org).

## [Unreleased]

### Added

- Governance: `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md` (this file).

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

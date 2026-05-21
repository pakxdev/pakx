# pakx

[![release](https://img.shields.io/github/v/release/pakxdev/pakx?display_name=tag&sort=semver)](https://github.com/pakxdev/pakx/releases/latest)
[![license](https://img.shields.io/github/license/pakxdev/pakx)](./LICENSE)
[![docs](https://img.shields.io/badge/docs-pakx.dev%2Fdocs-blue)](https://pakx.dev/docs)
[![api](https://img.shields.io/badge/api-pakx.dev%2Fdocs%2Fapi-blue)](https://pakx.dev/docs/api)

> The universal package manager for AI agent context. **One binary. One manifest. Every agent.**

`pakx` is a tiny native CLI that installs **skills, MCP servers, subagents, prompts, slash commands, and hooks** across every AI agent on your machine (Claude Code, Cursor, Codex, Copilot, Windsurf, and more) from a single manifest (`agents.yml`) and lockfile (`agents.lock`).

It federates existing registries — the official MCP Registry, Smithery, and the first-party [pakx-registry](https://registry.pakx.dev) — instead of competing with them. Distribution is a **single static binary**: download, run, done. No Node, no Python, no runtime to manage.

## Status

**v0.1 — early access.** Working today:

| Command | What it does |
|---|---|
| `pakx init` | Interactive scaffolder for `agents.yml`. |
| `pakx add <id>` | Append a dep to the manifest; best-effort validation against the registry. |
| `pakx install` | Resolve every MCP dep via the federated registry, install into Claude Code's project-scoped `.mcp.json`, and write `agents.lock`. |
| `pakx list` | Show pinned lockfile entries with `[ok]` / `[drift]` against on-disk reality. `--json` for pipelines. |
| `pakx doctor` | 5-section health check (manifest, lockfile, drift, adapter detection, on-disk vs lockfile). |
| `pakx search <query>` | Federated search across all sources. `--json` for pipelines. |
| `pakx test` | Validate `agents.yml` without installing — resolves every `mcp:` dep against the federated registries (official MCP Registry + Smithery + pakx-registry; toggle with `--no-smithery` / `--no-pakx-registry`) and exits non-zero on the first failure. Other dep kinds (`skills:` / `subagents:` / `prompts:` / `commands:` / `hooks:`) are reported as `skip` until per-kind resolvers land. `--offline` checks against the lockfile only. Intended for CI / pre-commit. |
| `pakx info <owner>/<name>` | Read-only registry inspection — metadata + version list. `--json` for pipelines. |
| `pakx login` | GitHub-backed login. Validates an API token against `registry.pakx.dev/api/v1/whoami` and writes `~/.pakx/credentials.json` (mode 0600). |
| `pakx whoami` | Stored login, or live whoami (`--offline` skips the network). |
| `pakx pack` | Build a deterministic gzipped tarball from a `SKILL.md` directory. |
| `pakx publish` | `pack` → `POST` package → `PUT` tarball. `--dry-run` skips the upload. |
| `pakx unpublish <owner>/<name>@<version>` | `DELETE` (with grace-period tombstoning on the server side). |
| `pakx upgrade` (alias `pakx update`) | Check GitHub Releases for a newer pakx and print the channel-appropriate install command. |
| `pakx completion <shell>` | Emit shell-completion script for bash / zsh / fish / powershell / elvish. |
| `pakx config` | Print resolved CLI configuration — credentials path, cache dir, federated registry URLs. `--json` for pipelines. |

In the registry (live at [registry.pakx.dev](https://registry.pakx.dev)): public browse + signed-in user dashboard + API tokens. Stripe Connect for marketplace payouts is scaffolded but not enabled.

See [`crates/pakx`](./crates/pakx), [`crates/pakx-core`](./crates/pakx-core), [`crates/pakx-agents`](./crates/pakx-agents), [`crates/pakx-registry-client`](./crates/pakx-registry-client).

## Install

Every channel resolves to the same signed binary from the [v0.1.1 GitHub Release](https://github.com/pakxdev/pakx/releases/tag/v0.1.1) and verifies a sha256 before installing.

**macOS / Linux**

```sh
curl -fsSL https://pakx.dev/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://pakx.dev/install.ps1 | iex
```

**Homebrew (macOS · Linux)**

```sh
brew install pakxdev/tap/pakx
```

**Scoop (Windows)**

```powershell
scoop bucket add pakx https://github.com/pakxdev/scoop-pakx
scoop install pakx
```

**From source**

```sh
cargo install --git https://github.com/pakxdev/pakx --tag v0.1.1 --locked pakx
```

**Direct download** — prebuilt binaries for `darwin/linux/windows × arm64/x86_64` plus matching `.sha256` files are at <https://github.com/pakxdev/pakx/releases/latest>. Winget manifest lands once the Microsoft community repository PR is reviewed.

## Quick start

```sh
pakx init                                                       # interactive: creates agents.yml
pakx add io.github.modelcontextprotocol/server-filesystem       # add MCP server
pakx install                                                    # resolve + install + write lockfile
pakx list                                                       # show what's pinned
pakx doctor                                                     # diagnose drift / missing agents
pakx search github                                              # browse the federated registry
```

After `pakx install`, Claude Code picks up new MCP servers from `<project>/.mcp.json` automatically.

### Publish your own package

```sh
pakx login                                                      # one-time
cd path/to/skill                                                # contains SKILL.md
pakx pack                                                       # dry-run: builds <name>-<version>.tgz
pakx publish                                                    # upload to registry.pakx.dev
```

Manage tokens at [registry.pakx.dev/dashboard/tokens](https://registry.pakx.dev/dashboard/tokens). Tokens are hashed at rest and shown once at issue.

## Build from source

Requires Rust 1.87+ (toolchain pinned to `stable` via `rust-toolchain.toml`).

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

GitHub Actions is temporarily disabled to control CI billing. Verification is local-first until a release tag is cut.

## Crates

| Crate | Description |
|---|---|
| `pakx` | The binary you install |
| `pakx-core` | Manifest, lockfile, install payloads, integrity hashing, credential store |
| `pakx-agents` | Adapters for Claude Code, Cursor, Codex, Copilot, Windsurf |
| `pakx-registry-client` | Federated index queries (MCP Registry, Smithery, pakx-registry) + authed `pakx_backend` client for publish/login |

## Contributing

PRs welcome. Every change goes through a feature branch + PR + squash merge (no direct main pushes). Local checks before pushing:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI matrix on ubuntu / macos / windows re-enables when the release pipeline lands.

## License

MIT — see [LICENSE](./LICENSE).

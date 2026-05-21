# pakx

> The universal package manager for AI agent context. **One binary. One manifest. Every agent.**

`pakx` is a tiny native CLI that installs **skills, MCP servers, subagents, prompts, slash commands, and hooks** across every AI agent on your machine (Claude Code, Cursor, Codex, Copilot, Windsurf, and more) from a single manifest (`agents.yml`) and lockfile (`agents.lock`).

It federates existing registries — the official MCP Registry, Smithery, Glama, GitHub-hosted skill repos — instead of competing with them. Distribution is a **single static binary**: download, run, done. No Node, no Python, no runtime to manage.

## Status

**v0.1 in active build.** Working subcommands today:

| Command | What it does |
|---|---|
| `pakx init` | Interactive scaffolder for `agents.yml`. |
| `pakx add <id>` | Append a dep to the manifest; best-effort validation against the official MCP Registry. |
| `pakx install` | Resolve every MCP dep via the federated registry, install into Claude Code's project-scoped `.mcp.json`, and write `agents.lock`. |
| `pakx list` | Show pinned lockfile entries with `[ok]` / `[drift]` against on-disk reality. |
| `pakx doctor` | 5-section health check (manifest, lockfile, drift, adapter detection, on-disk vs lockfile). |
| `pakx search <query>` | Federated search across registered sources. |

In progress:
- Smithery + GitHub-raw skill source (so `pakx install` covers skills too).
- `pakx login` / `pakx pack` / `pakx publish` (Phase C — needs the registry backend).
- `pakxdev/pakx-registry` (Phase B — Next.js + Vercel Postgres + Vercel Blob, hosts publish/auth/private packages).
- Web dashboard at [pakx.dev](https://pakx.dev) (Phase D).
- Stripe Connect marketplace payouts (Phase E).

See [`crates/pakx`](./crates/pakx), [`crates/pakx-core`](./crates/pakx-core), [`crates/pakx-agents`](./crates/pakx-agents), [`crates/pakx-registry-client`](./crates/pakx-registry-client).

## Install (preview — wired up once first release ships)

**macOS / Linux**

```sh
curl -fsSL https://pakx.dev/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://pakx.dev/install.ps1 | iex
```

**Homebrew (macOS + Linux)**

```sh
brew install pakxdev/tap/pakx
```

**Scoop (Windows)**

```powershell
scoop bucket add pakx https://github.com/pakxdev/scoop-pakx
scoop install pakx
```

**Winget (Windows)**

```powershell
winget install pakxdev.pakx
```

**Direct download:** prebuilt binaries for every supported OS / arch are on the [Releases](https://github.com/pakxdev/pakx/releases) page.

## Quick start

```sh
pakx init                                       # interactive: creates agents.yml
pakx add io.github.modelcontextprotocol/server-filesystem  # add MCP server
pakx install                                    # resolve + install + write lockfile
pakx list                                       # show what's pinned
pakx doctor                                     # diagnose drift / missing agents
pakx search github                              # browse the federated registry
```

After `pakx install`, Claude Code picks up new MCP servers from `<project>/.mcp.json` automatically.

## Build from source

Requires Rust 1.87+ (toolchain pinned to `stable` via `rust-toolchain.toml`).

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Crates

| Crate | Description |
|---|---|
| `pakx` | The binary you install |
| `pakx-core` | Manifest, lockfile, install payloads, integrity hashing |
| `pakx-agents` | Adapters for Claude Code, Cursor, Codex, Copilot, Windsurf |
| `pakx-registry-client` | Federated index queries (MCP Registry, Smithery, Glama, GitHub) |

## Contributing

PRs welcome. Every change goes through a feature branch + PR + squash auto-merge (no direct main pushes). CI runs `fmt`, `clippy --all-targets -D warnings`, and the test matrix on ubuntu / macos / windows for every commit.

## License

MIT — see [LICENSE](./LICENSE).

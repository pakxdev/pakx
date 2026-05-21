# pakx

> The universal package manager for AI agent context. **One binary. One manifest. Every agent.**

`pakx` is a tiny native CLI that installs **skills, MCP servers, subagents, prompts, slash commands, and hooks** across every AI agent on your machine (Claude Code, Cursor, Codex, Copilot, Windsurf, and more) from a single manifest (`agents.yml`) and lockfile (`agents.lock`).

It federates existing registries — the official MCP Registry, Smithery, Glama, GitHub-hosted skill repos — instead of competing with them. Distribution is a **single static binary**: download, run, done. No Node, no Python, no runtime to manage.

## Status

**v0.0.0 — scaffold only.** Commands are stubs while the underlying engine is built.

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

## Quick start (preview)

```sh
pakx init                     # interactive: creates agents.yml
pakx add pdf                  # add + install anthropics/skills/pdf
pakx add smithery/github-mcp  # add + install GitHub MCP server
pakx install                  # idempotent reinstall from manifest
pakx list                     # show what's installed where
```

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
| `pakx-core` | Manifest, lockfile, resolver, installer logic |
| `pakx-agents` | Adapters for Claude Code, Cursor, Codex, Copilot, Windsurf |
| `pakx-registry-client` | Federated index queries (MCP Registry, Smithery, Glama, GitHub) |

## License

MIT — see [LICENSE](./LICENSE).

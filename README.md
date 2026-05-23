# pakx

[![release](https://img.shields.io/github/v/release/pakxdev/pakx?display_name=tag&sort=semver)](https://github.com/pakxdev/pakx/releases/latest)
[![license](https://img.shields.io/github/license/pakxdev/pakx)](./LICENSE)
[![docs](https://img.shields.io/badge/docs-pakx.dev%2Fdocs-blue)](https://pakx.dev/docs)

pakx is a federated package manager for AI agent context — skills, MCP servers, subagents, prompts, slash commands, and hooks — installed from `registry.pakx.dev` and other federated sources into the agents already on your machine (Claude Code, Cursor, Codex, Copilot, Windsurf).

Distribution is a single static binary. A project declares its dependencies in `agents.yml`, and `pakx install` resolves them, verifies sha256 integrity, and writes the agent-specific config (for example `.mcp.json` for Claude Code) plus an `agents.lock` for reproducible installs.

## Install

The one-liners below download a prebuilt binary from the [v0.1.3 release](https://github.com/pakxdev/pakx/releases/tag/v0.1.3) and verify a script-pinned sha256 before installing.

macOS / Linux:

```sh
curl -fsSL https://pakx.dev/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://pakx.dev/install.ps1 | iex
```

Homebrew (macOS · Linux):

```sh
brew install pakxdev/tap/pakx
```

Scoop (Windows):

```powershell
scoop bucket add pakx https://github.com/pakxdev/scoop-pakx
scoop install pakx
```

From source (requires Rust 1.87+):

```sh
cargo install --git https://github.com/pakxdev/pakx --tag v0.1.3 --locked pakx
```

`cargo install pakx` from crates.io is not yet published; the GitHub-tag install above is the supported source path until then.

Prebuilt binaries plus matching `.sha256` files for `darwin / linux / windows × arm64 / x86_64` are at <https://github.com/pakxdev/pakx/releases/latest>.

## Quick start

```sh
pakx init                                                        # scaffold agents.yml
pakx add skills arwenizEr/hello-world                            # add a skill
pakx install                                                     # resolve, verify, write lockfile
pakx list                                                        # show what's pinned
```

After `pakx install`, detected agents pick up the new dependencies on their next launch. For Claude Code that means MCP servers are wired into `<project>/.mcp.json`; for skills, files are placed where the agent expects them.

To publish a package of your own:

```sh
pakx login                                                       # GitHub device-grant flow
cd path/to/skill                                                 # contains SKILL.md
pakx pack                                                        # builds <name>-<version>.tgz
pakx publish                                                     # uploads to registry.pakx.dev
```

API tokens are issued + revoked from <https://registry.pakx.dev/dashboard/tokens>.

## Subcommand reference

Run `pakx <command> --help` for full flag detail.

- `pakx init` — scaffold `agents.yml` in the current directory.
- `pakx add <id>` — append a dep to `agents.yml`.
- `pakx remove <id>` — drop a shorthand dep from `agents.yml`.
- `pakx install` — resolve every dep, verify integrity, install to detected agents, write `agents.lock`.
- `pakx list` — print pinned lockfile entries with `[ok]` / `[drift]` against on-disk state.
- `pakx tree` — render the lockfile grouped by kind and registry source.
- `pakx why <id>` — explain where a dep came from (manifest + lockfile).
- `pakx outdated` — show lockfile entries whose source registry has a newer version (exits non-zero on drift).
- `pakx audit` — flag lockfile entries pinned to a deprecated registry version.
- `pakx doctor` — health-check the project + agent install state.
- `pakx search <query>` — federated search across MCP Registry, Smithery, and pakx-registry.
- `pakx test` — validate `agents.yml` without installing (CI / pre-commit).
- `pakx info <owner>/<name>` — print registry metadata + version list for a published package.
- `pakx login` — log in via GitHub device-authorization grant.
- `pakx whoami` — print the GitHub login pakx is authenticated as.
- `pakx pack` — build a deterministic gzipped tarball from a local skill bundle.
- `pakx publish` — pack + upload a skill bundle to pakx-registry.
- `pakx unpublish <owner>/<name>@<version>` — soft-delete a published version.
- `pakx update` (alias `up`) — rewrite `agents.yml` pins to a newer version, then reinstall.
- `pakx upgrade` — check GitHub Releases for a newer pakx CLI binary.
- `pakx completion <shell>` — emit shell completion for bash / zsh / fish / powershell / elvish.
- `pakx config` — print the resolved CLI configuration (paths, federated registry URLs).
- `pakx manifest get|set|delete <path>` — read or mutate `agents.yml` by dot-path (scripting surface).

Every command accepts `--color auto|always|never` to control ANSI output. Most data-producing commands accept `--json` for pipelines.

## Manifest format

`agents.yml` declares dependencies grouped by kind. Shorthand entries are `<owner>/<name>@<version>`; object entries support `git:` and `registry:` forms. An optional `sponsors:` block at the top level surfaces funding links on the package page.

```yaml
name: my-project
version: 1.0.0

agents:
  - claude-code

dependencies:
  skills:
    - arwenizEr/hello-world@0.1.2
  mcp:
    - io.github.modelcontextprotocol/server-filesystem@latest
  subagents: []
  prompts: []
  commands: []
  hooks: []

sponsors:
  - kind: github
    url: https://github.com/sponsors/arwenizEr
```

A runnable starter is at [`examples/agents-yml-starter/`](./examples/agents-yml-starter). A minimal publishable skill is at [`examples/hello-world/`](./examples/hello-world).

## Crates

| Crate | Description |
|---|---|
| [`pakx`](./crates/pakx) | The binary you install. |
| [`pakx-core`](./crates/pakx-core) | Manifest, lockfile, install payloads, integrity hashing, credential store. |
| [`pakx-agents`](./crates/pakx-agents) | Adapters for Claude Code, Cursor, Codex, Copilot, Windsurf. |
| [`pakx-registry-client`](./crates/pakx-registry-client) | Federated index queries + authed client for publish / login. |

## Links

- Docs: <https://pakx.dev/docs>
- Registry web: <https://pakx.dev>
- Registry API: <https://registry.pakx.dev/api/v1>
- Changelog: [CHANGELOG.md](./CHANGELOG.md)
- Security policy: [SECURITY.md](./SECURITY.md)

## License

MIT — see [LICENSE](./LICENSE).

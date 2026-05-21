# Contributing to pakx

Thanks for your interest. pakx is a Rust workspace; the docs below cover the local loop, the PR shape we expect, and the cross-repo contracts you'll bump into.

## Local setup

1. Install [rustup](https://rustup.rs); pakx pins `stable` via `rust-toolchain.toml`.
2. Clone and enter the repo:
   ```sh
   git clone https://github.com/pakxdev/pakx
   cd pakx
   ```
3. Verify the toolchain works end-to-end:
   ```sh
   cargo build --workspace
   cargo test --workspace
   ```

## Before you push

Run these locally — there is no CI today (workflows are temporarily disabled until release billing lands), so the burden is on the contributor:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Push only when all three pass. If you cannot run the full test suite on your platform (e.g. Windows-specific bin paths), note that explicitly in the PR body so the reviewer runs them.

## Branching + PRs

- One feature per branch. Branch name: `feat/<slug>`, `fix/<slug>`, `chore/<slug>`, or `docs/<slug>`.
- Open a PR against `main`. Squash-merge is the only allowed merge style.
- **Never push to `main` directly.** Branch protection enforces this.
- Don't `--amend` a pushed commit; create a fixup commit and squash on merge.

PR description should explain the **why**, not just the what — the diff already shows the what. If the change is cross-repo (e.g. wire format), say which other repos are touched and link the matching PRs.

## Layout

| Crate | Role |
|---|---|
| `crates/pakx` | The CLI binary + subcommands |
| `crates/pakx-core` | Manifest schema, lockfile, install payloads, credential store, integrity hashing |
| `crates/pakx-agents` | Per-agent adapters (Claude Code, Cursor, Codex, Copilot, Windsurf) |
| `crates/pakx-registry-client` | Federated index queries + authed `pakx_backend` client |

## Cross-repo contracts to know

- `pakx-registry-client::pakx_backend` and `pakx-registry-client::pakx_source` both target the API exposed by [`pakxdev/pakx-registry`](https://github.com/pakxdev/pakx-registry) (`/api/v1/...`). New endpoints land on the registry first; the CLI changes follow.
- The manifest + lockfile schemas in `pakx-core` are the source of truth that other crates (and other repos) parse — bumping them is a breaking change.
- The marketing site `pakxdev/pakx-web` carries the docs (`/docs`), legal pages, and `/explore` browser. Keep its TS port of `pakx-registry-client` in sync with the Rust client when wire formats change.

## Style

- Avoid premature abstractions; the resolver is more legible with three similar lines than one parameterised one.
- Write tests with `wiremock` against the upstream API surfaces; don't mock our own internal types.
- Errors carry a `source_tag` so log lines stay greppable (`source=official-mcp`, `source=pakx`, etc.). Follow the existing pattern.
- No new top-level dependencies without a one-line note in the PR explaining what dropped that we keep.

## Issue triage

Open issues live at https://github.com/pakxdev/pakx/issues. Tagging conventions:

- `area/cli` — the binary surface
- `area/registry-client` — federated index
- `area/agents` — per-agent adapters
- `kind/bug`, `kind/feat`, `kind/docs`
- `good first issue` for newcomer-friendly tasks

Security reports do **not** go in issues — see [SECURITY.md](./SECURITY.md).

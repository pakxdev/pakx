# Security policy

Thanks for helping keep pakx and its users safe.

## Reporting a vulnerability

**Please do not open a public GitHub issue for security reports.** Instead, use one of:

- [GitHub Security Advisory](https://github.com/pakxdev/pakx/security/advisories/new) — preferred. Gives us a private channel and a coordinated-disclosure timeline.
- Email **security@pakx.dev**. PGP key on request.

Please include:

- The affected version (output of `pakx --version`) and platform.
- A reproducer — the smallest manifest, command, or HTTP request that triggers the issue.
- The impact you observed.

We aim to acknowledge new reports within **2 business days** and to ship a patch within **30 days** for high-severity issues. We'll keep you updated and credit you in the release notes unless you ask otherwise.

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x (early access) | ✅ |
| < 0.1 | ❌ |

While pakx is pre-1.0 only the latest minor receives security fixes. Upgrade ASAP when an advisory ships.

## Scope

In scope:

- The `pakx` CLI workspace in this repository.
- The `pakx-registry` backend at https://registry.pakx.dev (`pakxdev/pakx-registry`).
- The `pakx.dev` marketing site (`pakxdev/pakx-web`) — note that the bulk of its surface is static.
- Any prebuilt release binary or shell installer hosted under `pakx.dev` or `github.com/pakxdev`.

Out of scope:

- Third-party MCP servers, skills, or other published packages — report those to their authors.
- Bugs in upstream registries (`registry.modelcontextprotocol.io`, `registry.smithery.ai`) that pakx merely surfaces.
- Issues that require physical access to a user's machine or a compromised admin shell.

## Safe-harbour

We will not pursue legal action against anyone who reports a vulnerability in good faith, makes a reasonable effort to avoid privacy violations and data destruction, and does not exfiltrate data beyond what is needed to demonstrate the issue.

## Disclosure timeline

Default coordinated disclosure: **90 days** from the initial report, or earlier once a fix is released and broadly deployed. We're happy to align with an existing CVE / coordinated disclosure process you're already running.

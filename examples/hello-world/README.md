# hello-world

## what this is

A minimal pakx skill template. Copy it, rename it, and publish it to ship
your first skill to [registry.pakx.dev](https://registry.pakx.dev). The
sibling [`SKILL.md`](./SKILL.md) is the file the CLI actually packs — its
YAML frontmatter (`name`, `version`, `description`) becomes the package
manifest, and the markdown body is the prose agents see at install time.

## quick publish

```sh
cp -r examples/hello-world my-skill && cd my-skill
# edit SKILL.md: change `name:`, `version:`, `description:` in frontmatter
pakx login
pakx publish
```

Four steps:

1. Copy the directory and rename it to your skill's name.
2. Edit the frontmatter at the top of `SKILL.md`. `name:` is the slug
   that appears as `<your-github-login>/<name>` in the registry;
   `version:` is the semver string the publish will be pinned at;
   `description:` is the one-line summary surfaced in search results.
3. `pakx login` once per machine — opens a browser to mint an API
   token against your GitHub-backed registry account.
4. `pakx publish` tarballs the directory, uploads to the registry, and
   prints the resolved `<owner>/<name>@<version>` plus a sha256.

Run `pakx pack` first if you want to inspect the tarball before
uploading — it's the same code path `publish` uses, minus the network.

## adding sponsor links

Sponsorship links live in the `SKILL.md` frontmatter under `sponsors:`.
Up to five entries; each is a `{kind, url}` pair. Supported kinds:
`github`, `polar`, `kofi`, and a generic `url` escape hatch (https-only,
≤ 256 chars).

```yaml
---
name: hello-world
version: 0.1.0
description: A minimal pakx skill template.
sponsors:
  - kind: github
    url: https://github.com/sponsors/octocat
  - kind: url
    url: https://example.com/donate
---
```

`pakx pack` rejects malformed entries locally; the registry re-validates
server-side. Sponsor links render on the package's public page on
[pakx.dev/explore](https://pakx.dev/explore).

## what the registry does

`pakx publish` uploads the tarball to the pakx-registry, which records
the sha256, manifest, and metadata — every download is sha256-verified
by the CLI on install. Once published, the package is discoverable via
`pakx search`, `pakx info`, and the public browse UI at
[pakx.dev/explore](https://pakx.dev/explore).

## updating an existing publish

Bump `version:` in `SKILL.md` frontmatter (semver — `0.1.0` → `0.1.1` or
`0.2.0`) and re-run `pakx publish`. The registry rejects re-publishing
the same `<owner>/<name>@<version>` tuple; every release is immutable
once accepted.

## cleanup

```sh
pakx unpublish <owner>/<name>@<version>
```

Soft-deletes the version. It stops resolving for new installs
immediately but the tarball stays reachable for 30 days so existing
lockfiles keep working. After the grace window it returns 404.

---
name: hello-world
version: 0.1.0
description: A minimal pakx skill template. Copy it, rename it, publish it.
---

# hello-world

The smallest publishable pakx skill. Use it as a starting point for your
own skill packages.

## What's in here

- `SKILL.md` — the manifest + body. The YAML frontmatter is what
  `pakx pack` reads to derive `name`, `version`, and `description`.
- The body (everything below the frontmatter) is what AI agents see
  when this skill is in scope. Write it like a short manual: what
  the skill does, when to use it, what *not* to do.

## Publishing your fork

1. `cp -r examples/hello-world ~/projects/my-cool-skill`
2. Edit the frontmatter: change `name`, bump `version`, replace
   `description`.
3. Rewrite the body so it reflects what your skill actually does.
4. `pakx login` (one-time)
5. `pakx pack` — sanity-check the tarball locally.
6. `pakx publish` — uploads to `registry.pakx.dev`.

Your package is now installable by anyone:

```sh
pakx add skill <your-github-login>/my-cool-skill
pakx install
```

## Guidelines for writing a skill body

- One paragraph of context, then bullet points for the actual rules.
- Be specific. "Use `grep -r`" beats "use a tool to search".
- Tell the agent when **not** to use the skill — false positives
  cost more than false negatives.
- Keep it short. Agents read every word every time the skill is in
  scope.

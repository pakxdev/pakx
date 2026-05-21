## Summary

<!-- 1-3 sentences on what changes and WHY. The diff already shows what. -->

## Test plan

<!-- A bulleted markdown checklist. Include the exact commands you ran. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] (UI changes) verified the page renders in `next dev` / Vercel preview
- [ ] (cross-repo changes) linked the matching PRs in the related repos

## Cross-repo impact

<!-- If this PR changes a wire format, the manifest schema, the registry API
shape, or anything else other repos depend on, list those repos here.
Otherwise: "none." -->

none.

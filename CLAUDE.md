# Repository directives

## Version bumping

When making a code change that will be committed and merged, bump the
`version` field in `Cargo.toml` as part of the change:

- Default to a **patch** bump (e.g. `0.3.0` -> `0.3.1`).
- Use a **minor** bump for user-visible features, a **major** bump for
  breaking changes.
- Make the bump in the same commit/PR as the change, and run
  `cargo update -p aterm --offline` (or let the next build refresh it) so
  `Cargo.lock` stays in sync.

The release workflow (`.github/workflows/release.yml`) tags and publishes
`v<version>` based on whatever is in `Cargo.toml`, so an already-bumped
version is all that's needed to cut a release from the Actions tab.

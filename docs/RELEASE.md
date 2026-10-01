# Releasing

## Setup (one-time)

1. Create a crates.io API token at https://crates.io/settings/tokens (scope: **publish-update** for `qj`)
2. Add it as `CARGO_REGISTRY_TOKEN` in GitHub repo Settings → Secrets and variables → Actions

## Release process

1. Optionally, write the release notes as `docs/releases/v0.X.Y.md` and commit them to main
2. Go to Actions → Release → Run workflow
3. Enter the version (e.g. `0.1.1`)

The workflow will:
- Bump `Cargo.toml` version and commit
- Create and push a `v0.X.Y` tag
- Build binaries for macOS (x86_64, aarch64), Linux (x86_64, aarch64)
- Create a GitHub release with the tarballs, and `docs/releases/v0.X.Y.md` as its notes
  (auto-generated notes when there's no such file)
- Publish to crates.io

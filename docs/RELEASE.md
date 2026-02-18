# Releasing

## Setup (one-time)

1. Create a crates.io API token at https://crates.io/settings/tokens (scope: **publish-update** for `qj`)
2. Add it as `CARGO_REGISTRY_TOKEN` in GitHub repo Settings → Secrets and variables → Actions

## Release process

1. Go to Actions → Release → Run workflow
2. Enter the version (e.g. `0.1.1`)

The workflow will:
- Bump `Cargo.toml` version and commit
- Create and push a `v0.X.Y` tag
- Build binaries for macOS (x86_64, aarch64), Linux (x86_64, aarch64)
- Create a GitHub release with auto-generated notes and tarballs
- Publish to crates.io

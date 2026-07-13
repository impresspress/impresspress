# Releasing Impresspress

## Version Scheme

Impresspress uses [Semantic Versioning](https://semver.org/): `MAJOR.MINOR.PATCH`

- **MAJOR** — breaking changes to CLI flags, config format, or stored data
- **MINOR** — new features, new blocks, new config options
- **PATCH** — bug fixes, security patches, dependency updates

## Pre-Release Checklist

Before tagging a release, verify:

- [ ] `main` branch CI is green (check the [Actions tab](../../actions))
- [ ] Cross-platform builds pass (the `CI Main` workflow runs on every push to `main`)
- [ ] Update `version` in `Cargo.toml` workspace section to match the intended release
- [ ] No known critical bugs (check [open issues](../../issues))
- [ ] Test the binary locally:
  ```bash
  cargo build -p impresspress --release
  ./target/release/impresspress
  ```
- [ ] If this release changes config variables or CLI flags, update the docs

## Creating a Release

```bash
# 1. Make sure you're on main and up to date
git checkout main
git pull

# 2. Tag the release
git tag v0.2.0

# 3. Push the tag — this triggers the release workflow
git push origin v0.2.0
```

The [Release workflow](../../actions/workflows/release.yml) will automatically:
1. Build binaries for all 5 platforms (Linux amd64/arm64, macOS amd64/arm64, Windows amd64)
2. Create a GitHub Release with auto-generated notes from merged PRs

## After Release

- [ ] Verify the [GitHub Release](../../releases) was created with all 5 platform artifacts
- [ ] Download and smoke-test at least one binary
- [ ] Announce in relevant channels if this is a notable release

## Hotfix Process

Branch protection prevents pushing directly to `main` — hotfixes follow the same PR flow:

```bash
# 1. Create a hotfix branch
git checkout main && git pull
git checkout -b hotfix/v0.2.1

# 2. Fix the bug, commit, push
git push -u origin hotfix/v0.2.1

# 3. Open a PR — CI must pass, 1 approval required
gh pr create --title "fix: critical bug description"

# 4. After merge, tag the patch release
git checkout main && git pull
git tag v0.2.1
git push origin v0.2.1
```

## Undoing a Release

If a release was tagged by mistake or contains a critical issue:

```bash
# Delete the tag locally and remotely
git tag -d v0.2.0
git push origin --delete v0.2.0
```

Then delete the GitHub Release from the [Releases page](../../releases). Note: users who already downloaded the binary still have it.

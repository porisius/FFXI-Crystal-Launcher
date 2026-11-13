# Releasing

Pushing a version tag builds, signs and publishes a release
(`.github/workflows/release.yml`). Installed launchers find it on GitHub, check its signature
against the public key built into them, and update themselves.

Every push and pull request is built and tested by `.github/workflows/ci.yml`, which also keeps an
unsigned build of the launcher as a download on the run's page for 14 days.

## One-time setup: the signing key

Releases are signed with an Ed25519 key. The public half is built into the launcher
(`assets/update-public-key.hex`); the private half lives only in a GitHub secret and your backup.

1. On Windows, in this repository, create the key **outside** the repository:

   ```powershell
   cargo run --locked -- --create-launcher-update-key C:\Users\<you>\Keys\crystal-update.pk8
   ```

   It prints the public key: 64 hex characters.

2. Put that line in `assets/update-public-key.hex` (replacing the zeros) and commit it.

3. Add the private key as a repository secret named `CRYSTAL_UPDATE_KEY`
   (Settings → Secrets and variables → Actions → New repository secret), base64-encoded:

   ```powershell
   [Convert]::ToBase64String([IO.File]::ReadAllBytes("C:\Users\<you>\Keys\crystal-update.pk8")) | Set-Clipboard
   ```

4. Back up `crystal-update.pk8` somewhere safe (a password manager works), then you can delete the
   local copy. If it's lost, launchers already installed can't verify new releases: everyone would
   have to reinstall a build with a new key by hand.

`*.pk8` is in `.gitignore`, but check `git status` before committing anyway.

## Version numbers

This fork follows upstream's version with its own counter: `<upstream version>-lsb.<n>`.

| Release | Version |
|---|---|
| First build on upstream 1.2.7 | `1.2.7-lsb.1` |
| A fix of this fork's own, still on 1.2.7 | `1.2.7-lsb.2` |
| After merging upstream's 1.2.8 | `1.2.8-lsb.1` |

Exactly `-lsb.<n>` is a normal release: every installed launcher updates to it. Any other suffix
(`1.2.8-lsb.1.beta.1`) is a pre-release, offered only to launchers that are themselves running a
pre-release.

## Each release

1. Set the new version in `Cargo.toml` (`version = "1.2.7-lsb.2"`) and run `cargo update -w`
   so `Cargo.lock` follows (CI builds with `--locked` and stops if it doesn't). Commit.
2. Tag and push:

   ```
   git tag v1.2.7-lsb.2
   git push origin v1.2.7-lsb.2
   ```

3. The Release workflow checks that the tag matches `Cargo.toml` and that signing is set up,
   runs the tests, builds the launcher and the `Setup.exe` installer, signs the update package and
   publishes:

   | File | For |
   |---|---|
   | `Crystal-Launcher-<version>-Setup.exe` | New installs |
   | `Crystal-Launcher-<version>.zip` | The portable copy, and what launchers update from |
   | `launcher-update.json` | The signed description launchers check |
   | `SHA256SUMS.txt` | Checksums of the above |

A version with any suffix other than `-lsb.<n>` is marked as a pre-release on GitHub, and only
launchers running a pre-release are offered it.

## Merging upstream

```
git fetch upstream
git merge upstream/main
```

Upstream's merge sets `Cargo.toml` to its own version (say `1.2.8`); make it `1.2.8-lsb.1`
before releasing. Conflicts are most likely in `src/self_update.rs` (this fork's repository and
version rules) and the files that carry the private-server launching.

## Where launchers look

`src/self_update.rs` reads releases from `porisius/FFXI-Crystal-Launcher`. The workflows build
with `CRYSTAL_RELEASE_REPO` set to the repository they run in, so a fork of this fork publishes to
itself without code changes; it just needs its own key and secret.

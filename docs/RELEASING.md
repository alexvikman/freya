# Releasing Freya

How Freya is released, signed and updated, and how credentials are kept out of the
repository.

## Making a release

```sh
ci/release.sh 0.2.0            # Cargo.toml's version, the commit "Release 0.2.0", the tag v0.2.0
git push origin main v0.2.0    # starts the release workflow
```

`ci/release.sh` refuses unless the working copy is clean, on `main`, and the version is newer;
it scans every commit for secrets before it commits, and pushes nothing.

The tag starts [`.github/workflows/release.yml`](../.github/workflows/release.yml):

1. **check**: the tag is `vX.Y.Z`, matches the version in `Cargo.toml`, and is on `main`.
2. **ci**: all of [`ci.yml`](../.github/workflows/ci.yml) on the tagged commit: tests on
   macOS, Linux and Windows, the dependency checks, shellcheck, and gitleaks over every
   commit. Nothing is built unless it passes.
3. **macos**, **linux**, **windows**: build, package and sign on each platform (Apple Silicon
   and Intel; Linux x86-64 and arm64 on Ubuntu 22.04, so the AppImage runs widely; Windows
   x86-64), each with a smoke test of the version, giving the files named in *How it
   installs* below plus a `.dmg`, a `.deb` and an `.msi`.
4. **release**, which **waits for the owner's approval** (the `release` environment): writes
   `SHA256SUMS`, signs it with the release key, checks that signature against the key built
   into Freya (so a wrong or rotated key fails here, not in users' updaters), attests the
   build's provenance, and publishes the GitHub Release with notes from the commits since the
   last tag.

Started by hand (*Run workflow*), the workflow only builds the packages, as artifacts.

All actions are GitHub's own, pinned to commits (Dependabot keeps them current); no job keeps
the repository token, and only the release job may write.

### Once, on GitHub

- **Environment `release`** (Settings, Environments): the owner as required reviewer, and only
  `v*` tags may deploy to it.
- **Its secret `FREYA_RELEASE_SIGNING_KEY`**: the private release key, in OpenSSH format,
  straight from the password manager, e.g.
  `op read "op://<vault>/<item>/private key?ssh-format=openssh" | gh secret set FREYA_RELEASE_SIGNING_KEY --env release --repo alexvikman/freya`.
  If the key has a passphrase, also `FREYA_RELEASE_SIGNING_PASSPHRASE`.
- **With a Developer ID** (later): the variable `FREYA_MACOS_IDENTITY`, and the secrets
  `APPLE_CERTIFICATE` (the `.p12`, base64), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_API_KEY` (the
  `.p8`'s contents), `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`. The macOS jobs then sign with
  it and notarize. Keep them in an environment that only `v*` tags may use (and give the macOS
  jobs that `environment:`), not as repository secrets: started by hand, the workflow builds
  any branch, and repository secrets would reach it.
- **Last.fm**: the repository secrets `FREYA_LASTFM_API_KEY` and `FREYA_LASTFM_SECRET`, the
  key and shared secret of Freya's application on [Last.fm](https://www.last.fm/api/accounts),
  set from the checkout's `.env` with `gh secret set -f .env --repo alexvikman/freya`. Only the
  packaging steps see them, and build them into Freya; without them a release has scrobbling
  off. Repository secrets rather than an environment's: they end up in every published build,
  where any desktop app's Last.fm secret can be read, so they identify Freya rather than guard
  anything. To change them, make a new application on Last.fm and set both again.
- **Actions** (Settings, Actions, General): only actions by GitHub allowed, every action pinned
  to a full commit SHA, workflow permissions read-only, and approval required for workflows
  from all outside contributors.
- **Ruleset "Protect main"** (Settings, Rules): `main` cannot be deleted or force-pushed.

### Checking a release by hand

```sh
# allowed_signers: the key from freya/src/update/signing.rs
echo 'freya-release namespaces="freya-release" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGb2PDcF4r6PkdFcm4pzbCxzZPZuDls5ZJSICMFe91Bd' > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I freya-release -n freya-release -s SHA256SUMS.sig < SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS      # macOS: shasum -a 256 --check --ignore-missing
gh attestation verify Freya-0.2.0-macos-aarch64.dmg -R alexvikman/freya
```

## Secrets

Signing and publishing bring real credentials close to the repository: signing passwords,
Apple app-specific passwords and API keys, the private key that signs releases. None of them
may ever be committed, so every commit is scanned:

- **Before each commit**, `.githooks/pre-commit` runs [gitleaks](https://github.com/gitleaks/gitleaks)
  on the staged changes and refuses the commit if it finds a secret. Turn it on once per clone
  with `git config core.hooksPath .githooks`; it needs gitleaks installed
  (`brew install gitleaks`) and refuses to commit without it.
- **In CI**, the `secrets` job of `.github/workflows/ci.yml` scans every commit; locally,
  `ci/check.sh secrets` does the same in Docker.
- **Before each release**, the release workflow runs all of CI again, scan included, before
  anything is built or published.

`.gitleaks.toml` holds the configuration: gitleaks' own rules (private keys, cloud and GitHub
tokens and many more), plus one for Apple app-specific passwords. No path is skipped: Freya
only scans commits, and a skipped path would hide a secret committed there. `.gitignore`
keeps build output (`target/`), agent worktrees and settings (`.claude/`), key and
certificate files (`*.p12`, `*.p8`, `*.pem`, `*.key`, `*.cer`) and `.env` files out of
commits.

Local builds read Freya's Last.fm API key from `.env` in the checkout, which Git ignores:

```sh
FREYA_LASTFM_API_KEY=<the application's API key>
FREYA_LASTFM_SECRET=<its shared secret>
```

After creating or changing it, `touch freya/build.rs` so the next build reads it. Without it,
Freya builds and runs with scrobbling off.

Credentials are only ever referred to, never written down: as environment variables set by
the CI's secret store, a keychain profile (`xcrun notarytool store-credentials`) or a password
manager reference.

If gitleaks finds something that is not a secret (a test vector, a public key), prefer changing
the text so it no longer looks like one. If it must stay, add a `gitleaks:allow` comment on the
line, which gitleaks then skips, and say in the commit why it is safe.

If a real secret was committed: treat it as leaked even if the commit was never pushed. Revoke
or rotate it first, then remove it from the history.

## Signing

### macOS

`ci/package-macos.sh` builds `Freya.app` with cargo-packager, signs it with the hardened
runtime, checks the signature (`codesign --verify --deep --strict`) and writes, into
`target/dist/`:

- `Freya-<version>-macos-<arch>.app.zip`, which Freya's updater installs;
- `Freya-<version>-macos-<arch>.dmg`, the app and a link to Applications, for installing by
  hand.

Today the signature is **ad hoc** (`FREYA_MACOS_IDENTITY` unset). That is enough for an app
Freya's updater installs: it is downloaded by Freya, not a browser, so it is never quarantined
and Gatekeeper does not check it. A `.dmg` or `.zip` downloaded with a browser is quarantined,
and Gatekeeper refuses to open it without a right-click and Open (or, on recent macOS, a trip
to System Settings, Privacy & Security).

With an Apple Developer ID, nothing but the environment changes:

| Variable | |
|---|---|
| `FREYA_MACOS_IDENTITY` | The certificate's name in the keychain, e.g. `Developer ID Application: Name (TEAMID)`. Turns on the secure timestamp and signs the `.dmg` too |
| `FREYA_NOTARY_PROFILE` | A keychain profile, made once with `xcrun notarytool store-credentials`, holding an Apple ID and app-specific password or an API key; for releasing from a Mac |
| `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH` | An App Store Connect API key instead, as CI uses; the `.p8` file comes from the CI's secret store, written to a temporary file |

With both an identity and credentials, the app and the `.dmg` are notarized and their tickets
stapled, so downloads open without a warning. An identity without credentials signs but does
not notarize, and says so.

Freya needs no entitlements under the hardened runtime: it only plays audio (no microphone),
and its network use (radio, updates) is not restricted outside the App Sandbox, which Freya
does not use.

### Windows

Not signed yet, so SmartScreen warns the first time Freya is started from a download. The
release workflow has a step where a signing service (e.g. Azure Trusted Signing) or a
certificate can be plugged in.

### Linux

Packages are not signed one by one. Like every release file, they are covered by the signed
`SHA256SUMS` (see Updates below).

## Updates

Freya reads its releases from GitHub (`api.github.com/repos/alexvikman/freya/releases`),
passing over drafts, pre-releases and tags that are not `vX.Y.Z`, and offers the newest when it
is newer than itself. It looks at launch (after ten seconds) and then once a day, unless the
user turns that off; a version the user skipped is only offered again when they ask. Nothing is
installed without the user's click. The code is in `freya/src/update/`.

### What Freya trusts

Every release carries `SHA256SUMS`, the checksums of all its files, and `SHA256SUMS.sig`, a
signature over it made with `ssh-keygen -Y sign -n freya-release`. Before installing, Freya

1. checks the signature against the public keys built into it (`RELEASE_KEYS` in
   `freya/src/update/signing.rs`, verified with `ring`), and refuses a release without one;
2. downloads its package and checks it against its line in `SHA256SUMS`;
3. on macOS, also checks the new app's code signature, which seals the whole bundle.

The private key is not in the repository nor on any machine: it is kept in the owner's password
manager and in the GitHub environment `release`, which needs the owner's approval before a
workflow may use it. Someone who can push to the repository or edit a release therefore still
cannot make Freya install something else.

To replace the key, add the new public key to `RELEASE_KEYS` next to the old one, release, and
only then sign with the new key; remove the old key a release later.

### How it installs

| Installed as | Package | What Freya does |
|---|---|---|
| macOS app bundle | `Freya-<version>-macos-<arch>.app.zip` | Unpacks it next to the app, checks its signature, moves the running app aside and the new one into its place (moving the old one back if that fails), and restarts |
| Linux AppImage | `Freya-<version>-linux-<arch>.AppImage` | Writes it next to the running image and renames it over it |
| `.deb`, Windows, or run from a build | — | Shows the update; *Download* opens the release page |

The new Freya is started once the old one has quit, with the old one's environment, and
removes what the update left beside the app. A list of releases at the address in `FREYA_UPDATE_FEED`
takes the place of GitHub's, to try a release before publishing it; its files must still carry
a valid signature.

### Tested

- Unit tests: signatures (valid, changed, other key, other namespace, cut short), the list of
  releases and checksums, a release found, verified and installed from a local server, and
  refused when the package was changed or `SHA256SUMS` signed with another key; on macOS, a
  signed app swapped in and one changed after signing refused.
- End to end on macOS (2026-10-17): Freya 0.1.0, packaged and signed by
  `ci/package-macos.sh`, found 0.1.1 on a local server (signed with a temporary test key, removed
  afterwards), installed it on the user's click and restarted as 0.1.1, with the old bundle
  cleaned up and the new one verified.

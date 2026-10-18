# Dependencies

Freya pulls in about 380 crates when built for macOS or Windows, and about 470 for Linux. About
260 of them come with Slint (windows, fonts, text layout, drawing, the `.slint` compiler),
about 95 with the audio engine and about 64 with the library. Each of them is code that runs on
the user's machine, and with build scripts and procedural macros, code that runs on the
developer's machine during the build. Malicious crates have reached crates.io before, mostly
with names close to popular ones. Three things guard against that.

## 1. Versions are pinned

`Cargo.lock` is checked in. It names the exact version and checksum of every crate, so a build
never picks up a new release by itself. A published version on crates.io can never be
replaced, only yanked, so a pinned version stays the code it was.

## 2. `cargo deny`: what is allowed in

[`deny.toml`](../deny.toml) is checked by `ci/check.sh deps` (also part of a plain
`ci/check.sh`). The check fails when:

- a crate has an entry in the [RustSec](https://rustsec.org) database: a known vulnerability,
  a crate found to be malicious, or one its authors have given up;
- a crate comes from anywhere but crates.io, such as a git repository;
- a crate's license is not on the list of permissive licenses, apart from Slint, which Freya
  uses under its royalty-free license (Slint is credited in the About window);
- a dependency has no version (`*`).

Some abandoned crates are deep in other crates' dependencies, with no known flaw and nothing
Freya can replace them with. They are listed under `ignore` in `deny.toml`, each with the
reason and what would remove it. Look at them again when updating.

## 3. `cargo vet`: what has been read

[`supply-chain/`](../supply-chain) records, for every crate version, that someone trusted has
audited it, or that Freya exempts it knowingly:

- `config.toml` lists whose audits are trusted (Mozilla, Google, the Bytecode Alliance, ISRG,
  Zcash, Embark Studios and Ariel OS, who publish theirs) and the exemptions;
- `imports.lock` holds their audits as they were fetched, so the check runs offline;
- `audits.toml` holds Freya's own audits and the publishers Freya trusts.

When this was set up (2026-10-11), 109 crates were covered by those audits and 4 partly; the
other 633 (counted for all platforms) were exempted as they were. An exemption is not a
review. It only means that a new crate, or any new version of an existing one, now stops the
check until someone looks at it.

Since then (2026-10-16):

- **Audited by Freya:** eight small crates or small version steps that Freya uses directly:
  anyhow, serde, log and objc2 (the step from the last audited version), and spin_on, ctrlc,
  directories and realfft (in full). Later the same day, zip's step from 9.0.1 (yanked) to
  9.0.4, which closes a path check that let a skin package write outside its folder; a test
  in `freya-skin` now guards that. These were AI-assisted reviews (by Claude) of every
  changed line, looking at `unsafe`, build scripts, file and network access; each entry's
  notes say what was found.
- **Trusted by publisher:** crates published by Microsoft's windows-rs maintainer
  (`kennykerr`), by Slint's release workflow (`github:slint-ui/slint`) and Slint's co-founder
  (`ogoffart`, Slint's helper crates), and by the Rust project (`rust-lang-owner`). Nobody
  reads millions of lines of Windows bindings; this records honestly that they are trusted
  for who publishes them, not reviewed. A version from anyone else still stops the check.

- **Replaced:** souvlaki (system media controls), a one-person crate that brought in an old
  zbus 3, `windows 0.44`, the unmaintained `derivative` (ignored in `deny.toml` until then)
  and `block 0.1.6`. Freya now talks to each system itself (`freya/src/gui/system_media/`),
  through crates it already had: `windows`, the zbus 5 that Slint brings (with its blocking
  API, no new crates), and the objc2 family, of which only `objc2-media-player` is new; it
  was audited (generated bindings, no build script). `Cargo.lock` went from 750 to 689
  crates; `cargo vet prune` dropped the exemptions and imported audits nothing used anymore.

That leaves 176 crates covered, 3 partly and 506 exempted. Of the 45 crates Freya depends on
directly, 21 are covered; the other 24 (among them symphonia, cpal, rusqlite, lofty, image,
ureq and zbus) are exempted.

On 2026-10-17, after the npm supply-chain attacks, the exemptions were worked down further:

- **The publishers our auditors trust:** Mozilla, ISRG, Ariel OS and the Bytecode Alliance
  already trust some publishers for the crates they publish (David Tolnay, Ed Page, Andrew
  Gallant, Alex Crichton, Josh Stone, Amanieu d'Antras, Manish Goregaokar, Alice Ryhl and
  others). Freya trusts the same publishers, for each crate `cargo vet suggest` named and only
  for that publisher's releases, until 2027-10-17. 121 crates.
- **Version steps:** 70 exempted versions that were a step of under 1,000 changed lines from a
  version those auditors had already audited, read in full (four AI-assisted reviewers, by
  Claude, each diff against cargo vet's safe-to-deploy criteria: every changed `unsafe` block,
  build scripts, procedural macros, file, network, process and environment use, encoded or
  obfuscated content). All 70 were recorded as delta audits; many were manifest-only, and
  several fix undefined behaviour (zerovec-derive, android_system_properties) or remove
  `unsafe` (pollster, half). One step was not certified: wgpu-hal 30.0.1 resolves three
  CoreGraphics colour-space constants with `libloading` and returns the address of each
  `kCGColorSpace…` variable as if it were the string (`Symbol<&CFString>` dereferenced): a
  memory-safety bug, not malice, reached only by an app that picks those colour spaces.
  wgpu-hal is not built for any of Freya's targets (it is in `Cargo.lock` for one of Slint's
  optional renderers), so it stays exempted with this note.
- **A capability scan of every remaining exemption:** `ci/capability-scan.py` reads the
  sources of each exempted version (it runs nothing) and lists process spawning, network and
  dynamic loading, references to credentials, encoded blobs and executables, and in build
  scripts and procedural macros the file writes, environment variables and addresses. Nothing
  pointed to malice. Prebuilt code it lists by its magic (executables, libraries, static
  archives, COFF objects) or its name: ring's 17 COFF objects, assembly it ships assembled
  for Windows and which is linked into `freya.exe` there; winapi's import libraries for the
  MinGW targets (about 2,800 `.a` files), used only by the cross-build in `ci/check.sh`, not
  by the MSVC release; and two test DLLs in libloading's tests, never built. These are taken
  as their authors published them, not read byte by byte. The scan fails when an exempted
  crate is not downloaded, so a missing one is never taken for a clean one. The credential hits were PEM and RSA names in TLS code and Windows constants; the one
  token read is `CRATES_IO_TOKEN` in smol_str's own publishing script, outside the build. No
  build script or macro uses the network. Build scripts start only build tools (`rustc` for
  its version, `clang`, `llvm-config`, `rustfmt`, `ninja`, `git rev-parse`), read only
  configuration variables, and write only to `OUT_DIR`, except under features Freya never
  enables (`update_bindings`, `gen-tests`, bindgen updates). skia-bindings, whose build
  script downloads prebuilt Skia, is not built for any of Freya's targets either.

That leaves 341 crates covered, 3 partly and 342 exempted. Run the scan again when exemptions
change, and read what it lists in build scripts and macros first.

What is left is large: about 300 crates in full (6 million lines by cargo vet's estimate,
most of it generated bindings and bundled C such as SQLite and Opus) and 41 larger version
steps. Work them down most-exposed first: what Freya calls directly and what parses files
from outside (symphonia, lofty, image, the M3U and CUE readers' dependencies, zip), then
build-time code, then the rest.

## Patched crates

A crate Freya needs changed is copied into [`vendor/`](../vendor) from the release it is
based on, changed there, and used instead of the published one through `[patch.crates-io]` in
`Cargo.toml`. Its changes are described in `vendor/README.md`. Being a local folder, it is
read as Freya's own code: `cargo vet` no longer asks about it, and its changes are reviewed
like any other change to Freya. When updating Slint, compare the release the copy is based on
with the one Slint then asks for, and move the changes over or drop the copy.

## Adding a dependency

Prefer what the standard library or an existing dependency already does. Before adding a
crate, look at who publishes it, how widely it is used, when it was last released and what it
pulls in (`cargo tree -p <crate>`). Then add it to `Cargo.toml` with a version, build, and run
`cargo deny check` and `cargo vet`. For a crate nobody trusted has audited, read its code (at
least `build.rs`, macros and `unsafe` blocks) and record it with `cargo vet certify`, or, for
a large crate from a well-known publisher, `cargo vet add-exemption` with a note why.

## Updating

Update as a step of its own, never in passing with other work:

```sh
ci/update-deps.sh            # everything, within the versions Cargo.toml allows
ci/update-deps.sh rusqlite   # one crate
```

The script lists every crate version that goes or comes, then runs both checks. New versions
that nobody trusted has audited stop at `cargo vet`. For each, look at what changed with
`cargo vet diff <crate> <old> <new>` (or `cargo vet inspect` for a new crate), then certify
it or add an exemption. Run the tests and `ci/check.sh`, and commit `Cargo.lock` and
`supply-chain/` together, with the list of changes in the commit message.

Moving to a new major version (changing `Cargo.toml`) works the same way, followed by the
usual build and test.

Rust itself is pinned in `rust-toolchain.toml`, so CI, releases and `ci/check.sh` build with the
same compiler. Moving to a newer Rust is a step of its own too: change `channel` there, fix
whatever new clippy lints find, and run the tests and `ci/check.sh`.

## Tools

`cargo install --locked cargo-deny cargo-vet` installs both. `ci/check.sh deps` uses pinned
versions (cargo-deny 0.20.2, cargo-vet 0.10.2) in Docker, so the check does not depend on
what is installed locally.

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

## Tools

`cargo install --locked cargo-deny cargo-vet` installs both. `ci/check.sh deps` uses pinned
versions (cargo-deny 0.20.2, cargo-vet 0.10.2) in Docker, so the check does not depend on
what is installed locally.

# Monkey's Audio fixtures

No encoder ships with Freya, so these files were made once, with a small throwaway program
using the `oxideav-ape` 0.0.4 crate's encoder (file version 3990). Each file was decoded by
`oxideav-ape` and by `ape-decoder` 0.3.2, which checks every frame's CRC; both returned exactly
the samples that went in. Neither crate is a dependency of Freya.

- `../sine.ape`, `sine-96k-24bit.ape`, `mono-22k.ape`: the WAV fixtures of the same names,
  encoded at the levels Normal, High and Extra High. `sine.ape` carries an APEv2 tag (Title
  "Sine Test", Artist "Freya", Album "Fixtures").
- `stereo-16-{fast,normal,high,extra-high,insane}.ape`, `stereo-24-insane.ape`,
  `stereo-8-high.ape`, `mono-8-normal.ape`, `mono-24-fast.ape`: 6000 samples per channel of the
  synthetic signal in `src/ape/tests.rs` (`signal`), at 44.1 kHz, in frames of 2048.
- `stereo-16-flags.ape`: the same signal at level Normal, with the first frame silent and the
  second with the left channel in both, so the encoder marks those frames instead of coding
  them.

Files before version 3.99 cannot be made with any encoder at hand; that format is tested
against real discs with `decodes_real_files` (ignored by default).

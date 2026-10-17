# Patched crates

Copies of crates from crates.io that Freya builds with changes of its own, through
`[patch.crates-io]` in the workspace's `Cargo.toml`. Each is the published release with the
changes below and nothing else, apart from the release's CI files, which are left out. To see
the changes, compare the folder with the release Cargo downloaded, e.g.
`diff -r ~/.cargo/registry/src/index.crates.io-*/softbuffer-0.4.8 vendor/softbuffer`.

## `softbuffer` 0.4.8

Slint's software renderer, which Freya uses on macOS, hands its frames to the window through
softbuffer. Upstream, on macOS, softbuffer:

- gave the renderer a new, empty buffer every frame (`age` 0), so the renderer drew the whole
  window even when one line of text changed;
- handed Core Animation that buffer as a new `CGImage` in device RGB, which Core Animation
  copied and color-converted on the CPU before showing it.

With a full-screen window on a Retina MacBook, that took about 26 ms of the main thread for every
frame, however little changed: while music played, the position redrawn once a second cost
2.5 % of a core on its own.

The patch, in `src/backends/cg.rs` and the macOS dependencies in `Cargo.toml`:

- The pixels are kept between frames (`age` 1), so Slint redraws only what changed.
- Frames reach the window server through up to three `IOSurface`s, tagged sRGB. Only the
  rectangles that changed since a surface was last filled are copied into it, and the window
  server shows it directly, converting colors on the graphics card. The colors are the same as
  before: screenshots differ by at most 1 in 255.

Measured with the release build while playing, at the same window size: the queue view went
from about 5 % to 1.9 % of a core, the full-screen visualizer with the spectrum from 6.2 % to
3.3 %.

Other systems are unchanged. When Slint or softbuffer present differently on macOS, check
whether this is still needed; drop the copy once upstream does the same.

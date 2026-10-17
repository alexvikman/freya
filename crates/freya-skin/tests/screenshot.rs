//! Renders the built-in skin and compares it with a reference image.
//!
//! Text uses the system font, which differs between operating systems, so this runs on macOS
//! only. After an intended visual change, update the reference with:
//! `UPDATE_SNAPSHOTS=1 cargo test -p freya-skin --test screenshot`

#![cfg(target_os = "macos")]

use std::path::Path;

use freya_skin::SkinSource;

/// Pixels may differ this much per channel (font smoothing varies between macOS versions)…
const CHANNEL_TOLERANCE: u8 = 48;
/// …and this share of pixels may differ more than that.
const MAX_DIFFERENT_PIXELS: f64 = 0.0001;

#[test]
fn builtin_skin() {
    let image = freya_skin::render::render(
        &SkinSource::Builtin,
        1100,
        720,
        2.0,
        freya_skin::sample::fill,
    )
    .expect("render");
    let actual = image::RgbImage::from_raw(image.width, image.height, image.pixels).unwrap();

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/builtin.png");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        actual.save(&path).unwrap();
        return;
    }
    let expected = image::open(&path).expect("reference image").to_rgb8();
    assert_eq!(expected.dimensions(), actual.dimensions(), "size changed");
    let different = expected
        .pixels()
        .zip(actual.pixels())
        .filter(|(a, b)| {
            a.0.iter()
                .zip(b.0)
                .any(|(x, y)| x.abs_diff(y) > CHANNEL_TOLERANCE)
        })
        .count();
    let share = different as f64 / f64::from(actual.width() * actual.height());
    if share > MAX_DIFFERENT_PIXELS {
        let out = std::env::temp_dir().join("freya-builtin.png");
        actual.save(&out).unwrap();
        panic!(
            "{:.2}% of pixels differ; actual image written to {}",
            share * 100.0,
            out.display()
        );
    }
}

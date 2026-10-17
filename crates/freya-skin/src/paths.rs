//! Path data for skins: pure functions behind the skin API's path callbacks, shared by the
//! app and the previews. Slint's `Path` keeps the aspect ratio of its viewbox, so every
//! function takes the box size and returns data that fills it exactly.

/// SVG path data for a waveform in a `width` × `height` box: -1 at the bottom, 1 at the top,
/// with a little margin.
pub fn wave_path(width: f32, height: f32, samples: &[f32]) -> String {
    if samples.len() < 2 || width <= 0.0 || height <= 0.0 {
        return format!("M0 {:.2} L{width:.2} {:.2}", height / 2.0, height / 2.0);
    }
    let mut path = String::with_capacity(samples.len() * 14);
    for (i, v) in samples.iter().enumerate() {
        let x = i as f32 / (samples.len() - 1) as f32 * width;
        let y = (0.5 - v.clamp(-1.0, 1.0) * 0.45) * height;
        let cmd = if i == 0 { 'M' } else { 'L' };
        path.push_str(&format!("{cmd}{x:.2} {y:.2} "));
    }
    path.trim_end().to_owned()
}

/// Bars take this much of their slot; the rest is the gap to the next bar.
const BAR_FILL: f32 = 0.75;

/// SVG path data for spectrum bars in a `width` × `height` box: one rectangle per value,
/// standing on the bottom edge, `value` × `height` tall (at least 1).
pub fn bars_path(width: f32, height: f32, values: &[f32]) -> String {
    bar_rects(width, height, values, |v| {
        let top = height - (v.clamp(0.0, 1.0) * height).max(1.0);
        Some((top, height))
    })
}

/// SVG path data for peak markers: a `thickness` tall line just above where each bar would
/// reach. Peaks near zero are left out.
pub fn peaks_path(width: f32, height: f32, values: &[f32], thickness: f32) -> String {
    bar_rects(width, height, values, |v| {
        let top = height - v.clamp(0.0, 1.0) * height - thickness - 1.0;
        (v > 0.02).then_some((top.max(0.0), (top + thickness).max(thickness)))
    })
}

/// One rectangle per value, spread evenly across `width`; `span` gives its top and bottom.
fn bar_rects(
    width: f32,
    height: f32,
    values: &[f32],
    span: impl Fn(f32) -> Option<(f32, f32)>,
) -> String {
    if values.is_empty() || width <= 0.0 || height <= 0.0 {
        // An empty path draws nothing.
        return String::new();
    }
    let slot = width / values.len() as f32;
    let bar = (slot * BAR_FILL).max(1.0);
    let mut path = String::with_capacity(values.len() * 40);
    for (i, &v) in values.iter().enumerate() {
        let Some((top, bottom)) = span(v) else {
            continue;
        };
        let x = i as f32 * slot;
        path.push_str(&format!(
            "M{x:.2} {top:.2} H{:.2} V{bottom:.2} H{x:.2} Z ",
            x + bar
        ));
    }
    path.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_stand_on_the_bottom() {
        assert_eq!(
            bars_path(100.0, 40.0, &[0.5, 0.0]),
            "M0.00 20.00 H37.50 V40.00 H0.00 Z M50.00 39.00 H87.50 V40.00 H50.00 Z"
        );
        assert_eq!(
            peaks_path(100.0, 40.0, &[0.5, 0.0], 2.0),
            "M0.00 17.00 H37.50 V19.00 H0.00 Z"
        );
        assert_eq!(bars_path(100.0, 40.0, &[]), "");
    }

    #[test]
    fn wave_path_spans_the_box() {
        let path = wave_path(100.0, 40.0, &[1.0, 0.0, -1.0]);
        assert_eq!(path, "M0.00 2.00 L50.00 20.00 L100.00 38.00");
        assert_eq!(wave_path(100.0, 40.0, &[]), "M0 20.00 L100.00 20.00");
    }
}

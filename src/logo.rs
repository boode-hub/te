//! The 4N house mark (D:\SS\4N-HQ\style\brand\logo.svg): the three-tomoe Sharingan, black iris,
//! everything else in the accent colour. Rasterised here so the build script (exe icon) and the
//! app (window icon, top-bar logo) share one drawing without an SVG library.

/// Tail of one tomoe, traced in the SVG's 128-unit canvas.
const TOMOE: &[(f32, f32)] = &[
    (54.7, 27.3), (54.7, 26.8), (54.8, 26.3), (54.9, 25.8), (55.0, 25.3), (55.1, 24.8), (55.2, 24.2), (55.4, 23.7),
    (55.6, 23.2), (55.8, 22.7), (56.0, 22.2), (56.3, 21.7), (56.6, 21.2), (56.9, 20.7), (57.3, 20.3), (57.7, 19.8),
    (58.1, 19.3), (58.5, 18.8), (58.9, 18.3), (59.4, 17.9), (59.9, 17.4), (60.4, 17.0), (61.0, 16.5), (61.6, 16.2),
    (62.3, 15.8), (62.9, 15.6), (63.6, 15.3), (64.4, 15.0), (65.1, 14.8), (65.9, 14.6), (66.7, 14.6), (67.5, 14.6),
    (68.3, 14.6), (69.1, 14.5), (70.0, 14.6), (70.8, 14.7), (71.7, 14.9), (72.5, 15.1), (73.3, 15.4), (74.2, 15.7),
    (75.0, 16.1), (75.8, 16.6), (76.5, 17.1), (77.4, 17.6), (78.2, 18.1), (79.0, 18.7), (79.7, 19.4), (80.5, 20.1),
    (81.0, 20.9), (81.0, 20.9), (79.5, 20.6), (78.3, 20.2), (77.2, 20.0), (76.0, 19.8), (75.0, 19.6), (73.9, 19.6),
    (72.9, 19.6), (71.9, 19.7), (70.9, 20.0), (69.9, 20.4), (69.1, 20.7), (68.4, 21.0), (67.9, 20.8), (67.5, 20.6),
    (67.0, 20.4), (66.6, 20.2), (66.1, 20.1), (65.7, 20.0), (65.2, 19.9), (64.7, 19.9), (64.2, 19.9), (63.8, 19.9),
    (63.3, 19.9), (62.8, 19.9), (62.3, 20.0), (61.9, 20.1), (61.4, 20.2), (61.0, 20.4), (60.5, 20.6), (60.1, 20.8),
    (59.6, 21.0), (59.2, 21.2), (58.8, 21.5), (58.4, 21.8), (58.1, 22.1), (57.7, 22.4), (57.4, 22.7), (57.0, 23.1),
    (56.8, 23.4), (56.5, 23.8), (56.2, 24.2), (56.0, 24.6), (55.8, 25.1), (55.6, 25.5), (55.4, 25.9), (55.2, 26.4),
    (55.1, 26.9), (55.0, 27.3),
];

fn inside_polygon(x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = TOMOE.len() - 1;
    for i in 0..TOMOE.len() {
        let ((xi, yi), (xj, yj)) = (TOMOE[i], TOMOE[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// None = transparent, Some(false) = iris black, Some(true) = accent.
fn sample(x: f32, y: f32) -> Option<bool> {
    let r = ((x - 64.0).powi(2) + (y - 64.0).powi(2)).sqrt();
    if r > 60.25 {
        return None;
    }
    if r > 55.75 || (r - 35.0).abs() < 1.6 || r < 14.0 {
        return Some(true);
    }
    for k in 0..3 {
        // Rotate the point back by 0/120/240 degrees and test it against the one traced tomoe.
        let a = -(k as f32) * std::f32::consts::TAU / 3.0;
        let (dx, dy) = (x - 64.0, y - 64.0);
        let (tx, ty) = (64.0 + dx * a.cos() - dy * a.sin(), 64.0 + dx * a.sin() + dy * a.cos());
        if (tx - 64.0).powi(2) + (ty - 29.0).powi(2) < 9.14f32.powi(2) || inside_polygon(tx, ty) {
            return Some(true);
        }
    }
    Some(false)
}

/// n×n RGBA pixels (straight alpha), 4×4 supersampled. The viewBox is `2 2 124 124`.
pub fn rgba(n: usize, accent: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * n * 4);
    for py in 0..n {
        for px in 0..n {
            let (mut hits, mut lit) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = 2.0 + (px as f32 + (sx as f32 + 0.5) / 4.0) * 124.0 / n as f32;
                    let y = 2.0 + (py as f32 + (sy as f32 + 0.5) / 4.0) * 124.0 / n as f32;
                    if let Some(a) = sample(x, y) {
                        hits += 1;
                        lit += a as u32;
                    }
                }
            }
            let c = |v: u8| if hits == 0 { 0 } else { (v as u32 * lit / hits) as u8 };
            out.extend_from_slice(&[c(accent[0]), c(accent[1]), c(accent[2]), (hits * 255 / 16) as u8]);
        }
    }
    out
}

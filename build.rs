//! Renders the Mangekyou Sharingan logo (assets/logo/itachi.svg) at build time: every icon size
//! for the exe, plus a 128 px RGBA image the app uses for its window icon and top-bar logo.

use resvg::{tiny_skia, usvg};

const SVG: &str = "assets/logo/itachi.svg";

fn render(tree: &usvg::Tree, n: u32) -> tiny_skia::Pixmap {
    let mut pm = tiny_skia::Pixmap::new(n, n).expect("pixmap");
    let s = tree.size();
    let k = n as f32 / s.width().max(s.height());
    resvg::render(tree, tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
    pm
}

fn main() {
    println!("cargo:rerun-if-changed={SVG}");
    let tree = usvg::Tree::from_data(&std::fs::read(SVG).expect("read logo"), &usvg::Options::default()).expect("parse logo");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    // Straight (not premultiplied) RGBA, which is what egui's IconData and ColorImage expect.
    let rgba: Vec<u8> = render(&tree, 128)
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    std::fs::write(out.join("logo_128.rgba"), rgba).expect("write logo");

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    let ico = out.join("te.ico");
    std::fs::write(&ico, ico_file(&tree, &[16, 20, 24, 32, 40, 48, 64, 96, 128, 256])).expect("write icon");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("utf-8 path"))
        .set("ProductName", "te")
        .set("FileDescription", "te text editor");
    if let Err(e) = res.compile() {
        // A missing resource compiler must not break the build; the exe just has no icon.
        println!("cargo:warning=could not embed the icon: {e}");
    }
}

/// An .ico file with one PNG image per size, each rendered from the vector at that exact size.
fn ico_file(tree: &usvg::Tree, sizes: &[u32]) -> Vec<u8> {
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&n| render(tree, n).encode_png().expect("png")).collect();
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (n, data) in sizes.iter().zip(&pngs) {
        let dim = if *n >= 256 { 0 } else { *n as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for d in pngs {
        out.extend_from_slice(&d);
    }
    out
}

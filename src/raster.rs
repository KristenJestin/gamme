//! The pages of a score as images: rasterized once per size, zoom and ink, never per frame
//! (about 100 ms for three pages). Off the main thread: the view puts the result in place.

use std::sync::Arc;

use gpui_kit::{RenderImage, Rgba};
use resvg::{tiny_skia, usvg};

use crate::score::Score;

/// Every page `width` device pixels wide, each on a thread of its own, drawn in `ink` on
/// nothing: the page under it shows through, and the score takes the theme's colours.
pub fn pages(score: &Score, width: u32, ink: Rgba) -> Vec<Arc<RenderImage>> {
    std::thread::scope(|scope| {
        let pages: Vec<_> = score
            .pages()
            .iter()
            .map(|tree| scope.spawn(move || page(tree, width, ink)))
            .collect();
        pages
            .into_iter()
            .map(|page| Arc::new(page.join().expect("a page rasterizes")))
            .collect()
    })
}

/// One page, `width` device pixels wide.
pub fn page(tree: &usvg::Tree, width: u32, ink: Rgba) -> RenderImage {
    let width = width.max(1);
    let scale = width as f32 / tree.size().width();
    let height = ((tree.size().height() * scale).ceil() as u32).max(1);
    let mut pixmap = tiny_skia::Pixmap::new(width, height).expect("a page has a size");
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // Verovio draws in one colour: what it covered is the ink, at that coverage. GPUI's images
    // are BGRA, not premultiplied.
    let [b, g, r] = [ink.b, ink.g, ink.r].map(|c| (c.clamp(0., 1.) * 255.).round() as u8);
    let mut data = pixmap.take();
    for pixel in data.as_chunks_mut::<4>().0 {
        let coverage = f32::from(pixel[3]) * ink.a.clamp(0., 1.);
        *pixel = [b, g, r, coverage.round() as u8];
    }
    let buffer = image::RgbaImage::from_raw(width, height, data).expect("the pixmap fits");
    RenderImage::new(vec![image::Frame::new(buffer)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20">
<rect x="0" y="0" width="5" height="20" fill="black"/>
</svg>"#;

    #[test]
    fn a_page_is_drawn_in_the_ink_at_its_width_and_shape() {
        let tree = usvg::Tree::from_str(SQUARE, &usvg::Options::default()).unwrap();
        let ink = Rgba {
            r: 1.,
            g: 0.5,
            b: 0.,
            a: 1.,
        };
        let image = page(&tree, 40, ink);
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (40, 80));
        let bytes = image.as_bytes(0).unwrap();
        let at = |x: usize, y: usize| &bytes[(y * 40 + x) * 4..][..4];
        // BGRA: the ink where the rectangle is, nothing where it is not.
        assert_eq!(at(5, 40), [0, 128, 255, 255]);
        assert_eq!(at(30, 40)[3], 0);
    }
}

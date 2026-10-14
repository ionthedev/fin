//! Render only the visible SVG region at the display's physical pixel density.

use resvg::{tiny_skia, usvg};

const MAX_PIXELS: u64 = 8_000_000;
const MAX_PIXEL_SIDE: u32 = 8_192;

/// Visible rectangle in unzoomed diagram coordinates. It may extend beyond SVG bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub zoom: f32,
    pub scale_factor: f32,
}

/// A premultiplied RGBA crop and its placement in unzoomed diagram coordinates.
#[derive(Debug)]
pub struct Preview {
    pub pixels: Vec<u8>,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Rasterize vectors at the current zoom and display scale, without allocating
/// pixels for offscreen portions of a large diagram. Only oversized viewports
/// reduce the resolution to respect the preview's memory and dimension limits.
pub fn rasterize(tree: &usvg::Tree, viewport: Viewport) -> Result<Option<Preview>, String> {
    let Viewport {
        x,
        y,
        width,
        height,
        zoom,
        scale_factor,
    } = viewport;
    if ![x, y, width, height, zoom, scale_factor]
        .into_iter()
        .all(f32::is_finite)
        || width < 0.0
        || height < 0.0
        || zoom <= 0.0
        || scale_factor <= 0.0
    {
        return Err("The diagram viewport has invalid dimensions or scale.".into());
    }
    if width == 0.0 || height == 0.0 {
        return Ok(None);
    }

    // Use f64 for clipping and pixel-grid arithmetic so adding two valid f32
    // inputs cannot overflow before their intersection with the SVG is known.
    let left = f64::from(x).max(0.0);
    let top = f64::from(y).max(0.0);
    let right = (f64::from(x) + f64::from(width)).min(f64::from(tree.size().width()));
    let bottom = (f64::from(y) + f64::from(height)).min(f64::from(tree.size().height()));
    if right <= left || bottom <= top {
        return Ok(None);
    }

    let crop_width = right - left;
    let crop_height = bottom - top;
    let requested_scale = f64::from(zoom) * f64::from(scale_factor);
    let mut scale = requested_scale
        .min((MAX_PIXELS as f64 / (crop_width * crop_height)).sqrt())
        .min(f64::from(MAX_PIXEL_SIDE) / crop_width.max(crop_height));
    // tiny-skia's transform uses f32. Align and place against that same scale
    // instead of subtly different f64 and f32 pixel grids at fractional zoom.
    scale = f64::from(scale as f32);

    // Outward rounding includes every partially visible physical pixel. After
    // a memory-limited reduction, reserve two pixels per side for that rounding;
    // the normal path keeps the requested density exactly.
    let mut bounds = pixel_bounds(left, top, right, bottom, scale);
    if exceeds_budget(bounds) {
        let area = crop_width * crop_height;
        let perimeter = 2.0 * (crop_width + crop_height);
        let available = MAX_PIXELS as f64 - 4.0;
        // Positive root of (width * scale + 2) * (height * scale + 2) = budget.
        // This form avoids cancellation for very tall or narrow diagrams.
        let area_scale =
            2.0 * available / (perimeter + (perimeter * perimeter + 4.0 * area * available).sqrt());
        scale = scale
            .min(area_scale)
            .min(f64::from(MAX_PIXEL_SIDE - 2) / crop_width.max(crop_height));
        scale = f64::from(scale as f32);
        bounds = pixel_bounds(left, top, right, bottom, scale);
    }
    if !scale.is_finite()
        || scale <= 0.0
        || !(scale as f32).is_finite()
        || (scale as f32) == 0.0
        || exceeds_budget(bounds)
        || !(bounds.0 as f32).is_finite()
        || !(bounds.1 as f32).is_finite()
    {
        return Err("The diagram viewport is too large to preview.".into());
    }

    let (pixel_left, pixel_top, pixel_right, pixel_bottom) = bounds;
    let pixel_width = (pixel_right - pixel_left) as u32;
    let pixel_height = (pixel_bottom - pixel_top) as u32;
    if pixel_width == 0 || pixel_height == 0 {
        return Ok(None);
    }
    let mut pixmap = tiny_skia::Pixmap::new(pixel_width, pixel_height)
        .ok_or_else(|| "Could not allocate the diagram viewport.".to_string())?;
    let transform = tiny_skia::Transform::from_row(
        scale as f32,
        0.0,
        0.0,
        scale as f32,
        -pixel_left as f32,
        -pixel_top as f32,
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());

    Ok(Some(Preview {
        pixels: pixmap.take(),
        pixel_width,
        pixel_height,
        x: (pixel_left / scale) as f32,
        y: (pixel_top / scale) as f32,
        width: (f64::from(pixel_width) / scale) as f32,
        height: (f64::from(pixel_height) / scale) as f32,
    }))
}

fn pixel_bounds(left: f64, top: f64, right: f64, bottom: f64, scale: f64) -> (f64, f64, f64, f64) {
    (
        (left * scale).floor(),
        (top * scale).floor(),
        (right * scale).ceil(),
        (bottom * scale).ceil(),
    )
}

fn exceeds_budget((left, top, right, bottom): (f64, f64, f64, f64)) -> bool {
    let width = right - left;
    let height = bottom - top;
    ![left, top, right, bottom].into_iter().all(f64::is_finite)
        || width > f64::from(MAX_PIXEL_SIDE)
        || height > f64::from(MAX_PIXEL_SIDE)
        || width * height > MAX_PIXELS as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(width: u32, height: u32, content: &str) -> usvg::Tree {
        usvg::Tree::from_str(
            &format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}">{content}</svg>"#
            ),
            &usvg::Options::default(),
        )
        .expect("test SVG")
    }

    fn viewport(width: f32, height: f32) -> Viewport {
        Viewport {
            x: 0.0,
            y: 0.0,
            width,
            height,
            zoom: 1.0,
            scale_factor: 1.0,
        }
    }

    #[test]
    fn high_zoom_and_dpi_resolve_details_absent_from_a_fixed_raster() {
        let tree = svg(32, 12, r#"<rect x="10.25" width="0.5" height="12"/>"#);
        let mut view = viewport(32.0, 12.0);
        view.scale_factor = 2.0;
        let low = rasterize(&tree, view).unwrap().unwrap();
        view.zoom = 3.0;
        let high = rasterize(&tree, view).unwrap().unwrap();
        assert_eq!((high.pixel_width, high.pixel_height), (192, 72));
        assert_eq!(high.pixels.len(), 192 * 72 * 4);
        // The half-unit line is only partially covered pixels in the old 2x
        // image, but has fully opaque interior pixels at 300% on a 2x display.
        assert!(
            low.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] < 200)
        );
        assert_eq!(high.pixels[(62 + 20 * 192) * 4 + 3], 255);
        assert_eq!(high.pixels[(60 + 20 * 192) * 4 + 3], 0);
    }

    #[test]
    fn clipped_viewport_matches_the_same_pixels_of_a_full_vector_render() {
        let tree = svg(
            100,
            80,
            r##"<rect width="100" height="80" fill="#182034"/><path d="M5 10L90 95M20 60L75 5" stroke="#e0a080" stroke-width="1.5"/><circle cx="45" cy="35" r="9" fill="#8575cc"/>"##,
        );
        let mut full_view = viewport(100.0, 80.0);
        full_view.zoom = 3.0;
        full_view.scale_factor = 2.0;
        let full = rasterize(&tree, full_view).unwrap().unwrap();
        let crop = rasterize(
            &tree,
            Viewport {
                x: 23.0,
                y: 17.0,
                width: 41.0,
                height: 36.0,
                ..full_view
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (crop.x, crop.y, crop.width, crop.height),
            (23.0, 17.0, 41.0, 36.0)
        );
        for row in 0..crop.pixel_height as usize {
            let full_start = ((row + 102) * full.pixel_width as usize + 138) * 4;
            let crop_start = row * crop.pixel_width as usize * 4;
            let bytes = crop.pixel_width as usize * 4;
            assert!(
                crop.pixels[crop_start..crop_start + bytes]
                    == full.pixels[full_start..full_start + bytes],
                "crop row {row} differs from the full raster"
            );
        }
    }

    #[test]
    fn fractional_viewports_align_outward_without_stretching_pixels() {
        let tree = svg(100, 100, r#"<rect width="100" height="100"/>"#);
        let view = Viewport {
            x: 10.13,
            y: 7.71,
            width: 30.27,
            height: 20.15,
            zoom: 1.3,
            scale_factor: 1.25,
        };
        let result = rasterize(&tree, view).unwrap().unwrap();
        let scale = view.zoom * view.scale_factor;
        assert_eq!((result.pixel_width, result.pixel_height), (50, 34));
        assert!((result.x * scale - 16.0).abs() < 0.0001);
        assert!((result.y * scale - 12.0).abs() < 0.0001);
        assert!((result.width * scale - 50.0).abs() < 0.0001);
        assert!((result.height * scale - 34.0).abs() < 0.0001);
        assert!(result.x <= view.x && result.y <= view.y);
        assert!(result.x + result.width >= view.x + view.width);
        assert!(result.y + result.height >= view.y + view.height);
    }

    #[test]
    fn large_offscreen_diagrams_do_not_reduce_visible_detail() {
        let tree = svg(
            100_000,
            100_000,
            r#"<rect x="90000" y="90000" width="20" height="20" fill="red"/>"#,
        );
        let result = rasterize(
            &tree,
            Viewport {
                x: 90_000.0,
                y: 90_000.0,
                zoom: 3.0,
                scale_factor: 2.0,
                ..viewport(100.0, 80.0)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!((result.pixel_width, result.pixel_height), (600, 480));
        assert_eq!(
            &result.pixels[4 * (60 + 60 * 600)..][..4],
            &[255, 0, 0, 255]
        );
        assert_eq!(result.pixels.len(), 600 * 480 * 4);
    }

    #[test]
    fn oversized_visible_regions_remain_within_memory_and_side_limits() {
        let tree = svg(100_000, 100_000, "");
        for (width, height) in [(100_000.0, 100_000.0), (100_000.0, 1.0)] {
            let result = rasterize(
                &tree,
                Viewport {
                    zoom: 3.0,
                    scale_factor: 2.0,
                    ..viewport(width, height)
                },
            )
            .unwrap()
            .unwrap();
            assert!(u64::from(result.pixel_width) * u64::from(result.pixel_height) <= MAX_PIXELS);
            assert!(result.pixel_width <= MAX_PIXEL_SIDE);
            assert!(result.pixel_height <= MAX_PIXEL_SIDE);
            assert_eq!(
                result.pixels.len(),
                result.pixel_width as usize * result.pixel_height as usize * 4
            );
            assert!(result.width >= width && result.height >= height);
        }
    }

    #[test]
    fn empty_and_invalid_viewports_are_handled_without_allocation() {
        let tree = svg(100, 100, "");
        for view in [
            viewport(0.0, 100.0),
            Viewport {
                x: 101.0,
                ..viewport(10.0, 10.0)
            },
            Viewport {
                x: -20.0,
                ..viewport(10.0, 10.0)
            },
        ] {
            assert!(rasterize(&tree, view).unwrap().is_none());
        }
        for view in [
            viewport(-1.0, 100.0),
            Viewport {
                x: f32::NAN,
                ..viewport(10.0, 10.0)
            },
            Viewport {
                height: f32::INFINITY,
                ..viewport(10.0, 10.0)
            },
            Viewport {
                zoom: 0.0,
                ..viewport(10.0, 10.0)
            },
            Viewport {
                scale_factor: -1.0,
                ..viewport(10.0, 10.0)
            },
        ] {
            assert!(rasterize(&tree, view).is_err());
        }
        let clipped = rasterize(
            &tree,
            Viewport {
                x: -10.0,
                y: -10.0,
                ..viewport(20.0, 20.0)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (clipped.x, clipped.y, clipped.width, clipped.height),
            (0.0, 0.0, 10.0, 10.0)
        );
    }
}

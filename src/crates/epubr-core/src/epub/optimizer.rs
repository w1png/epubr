use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{
    DynamicImage, ExtendedColorType, GenericImageView, GrayImage, ImageFormat, ImageReader, Luma,
};

use crate::OptimizationTarget;

const JPEG_QUALITY: u8 = 70;
const GRAYSCALE_STEP: u8 = 85;

pub(super) fn image_bounds(target: OptimizationTarget) -> (u32, u32) {
    match target {
        OptimizationTarget::XteinkX3 => (792, 528),
        OptimizationTarget::XteinkX4
        | OptimizationTarget::XteinkX4Pro
        | OptimizationTarget::SeeedStudioSticky => (800, 480),
    }
}

/// Return a screen-sized baseline JPEG, or keep the input if optimization is unavailable.
pub(super) fn optimize_image(
    target: OptimizationTarget,
    media_type: &str,
    data: &[u8],
) -> Option<(&'static str, Vec<u8>)> {
    let format = match media_type {
        "image/gif" => ImageFormat::Gif,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::WebP,
        _ => return None,
    };
    let image = ImageReader::with_format(Cursor::new(data), format)
        .decode()
        .ok()?;
    let (max_width, max_height) = image_bounds(target);
    let needs_resize = image.width() > max_width || image.height() > max_height;
    let mut already_grayscale = true;
    let gray = GrayImage::from_fn(image.width(), image.height(), |x, y| {
        let [red, green, blue, alpha] = image.get_pixel(x, y).0;
        let gray = ((299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue) + 500)
            / 1000) as u8;
        already_grayscale &=
            red == green && green == blue && alpha == 255 && gray.is_multiple_of(GRAYSCALE_STEP);
        let alpha = u16::from(alpha);
        let composited = (u16::from(gray) * alpha + 255 * (255 - alpha) + 127) / 255;
        Luma([composited as u8])
    });
    let mut gray = if needs_resize {
        DynamicImage::ImageLuma8(gray)
            .resize(max_width, max_height, FilterType::Lanczos3)
            .into_luma8()
    } else {
        gray
    };
    for pixel in gray.as_mut() {
        // Inky's browser defaults use 1.2× contrast around the midpoint,
        // followed by four equally spaced input bands.
        let contrasted = (((i16::from(*pixel) - 128) * 6 + 640) / 5).clamp(0, 255) as u8;
        *pixel = (contrasted / 64) * GRAYSCALE_STEP;
    }

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(
            gray.as_raw(),
            gray.width(),
            gray.height(),
            ExtendedColorType::L8,
        )
        .ok()?;
    // Keep small lossless diagrams when they already have a device grayscale palette.
    if format == ImageFormat::Png && !needs_resize && already_grayscale && jpeg.len() >= data.len()
    {
        return None;
    }
    Some(("image/jpeg", jpeg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgba, RgbaImage, codecs::png::PngEncoder};

    const X4: OptimizationTarget = OptimizationTarget::XteinkX4;

    fn png(image: &RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn fits_x4_screen_without_stretching() {
        for (width, height, expected) in [(1000, 500, (800, 400)), (600, 1000, (288, 480))] {
            let original = RgbaImage::from_pixel(width, height, Rgba([38, 72, 91, 255]));
            let (media_type, data) = optimize_image(X4, "image/png", &png(&original)).unwrap();

            assert_eq!(media_type, "image/jpeg");
            let resized = image::load_from_memory_with_format(&data, ImageFormat::Jpeg).unwrap();
            assert_eq!(resized.dimensions(), expected);
        }
    }

    #[test]
    fn uses_each_readers_display_bounds() {
        let original = png(&RgbaImage::from_pixel(900, 600, Rgba([38, 72, 91, 255])));
        for (target, expected) in [
            (OptimizationTarget::XteinkX3, (792, 528)),
            (X4, (720, 480)),
            (OptimizationTarget::XteinkX4Pro, (720, 480)),
            (OptimizationTarget::SeeedStudioSticky, (720, 480)),
        ] {
            let (_, data) = optimize_image(target, "image/png", &original).unwrap();
            let resized = image::load_from_memory_with_format(&data, ImageFormat::Jpeg).unwrap();
            assert_eq!(resized.dimensions(), expected);
        }
    }

    #[test]
    fn flattens_transparency_and_quantizes_without_upscaling() {
        let original = RgbaImage::from_fn(32, 16, |x, _| match x / 8 {
            0 => Rgba([0, 0, 0, 255]),
            1 => Rgba([75, 75, 75, 255]),
            2 => Rgba([150, 150, 150, 255]),
            _ => Rgba([0, 0, 0, 0]),
        });
        let (_, data) = optimize_image(X4, "image/png", &png(&original)).unwrap();
        let image = image::load_from_memory_with_format(&data, ImageFormat::Jpeg)
            .unwrap()
            .into_luma8();

        assert_eq!(image.dimensions(), (32, 16));
        for (x, level) in [(4, 0), (12, 85), (20, 170), (28, 255)] {
            assert!(image.get_pixel(x, 8).0[0].abs_diff(level) <= 3);
        }
    }

    #[test]
    fn retains_unsupported_invalid_and_small_device_grayscale_images() {
        assert!(optimize_image(X4, "image/svg+xml", b"<svg/>").is_none());
        assert!(optimize_image(X4, "image/png", b"invalid PNG").is_none());
        let diagram = RgbaImage::from_fn(8, 8, |x, _| {
            let gray = if x < 4 { 0 } else { 255 };
            Rgba([gray, gray, gray, 255])
        });
        assert!(optimize_image(X4, "image/png", &png(&diagram)).is_none());
    }
}

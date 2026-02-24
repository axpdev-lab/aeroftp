// ─── AeroImage: Image editing pipeline ──────────────────────────────────────
//
// Provides a single `process_image` command that accepts a pipeline of
// operations (crop, resize, rotate, flip, color adjustments, filters)
// and saves the result to the specified output path and format.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use serde::{Deserialize, Serialize};
use std::io::BufWriter;

use crate::filesystem::validate_path;

/// An image editing operation to apply in sequence.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ImageOperation {
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Rotate90,
    Rotate180,
    Rotate270,
    FlipH,
    FlipV,
    Brightness {
        value: i32,
    },
    Contrast {
        value: f32,
    },
    Blur {
        sigma: f32,
    },
    Sharpen {
        sigma: f32,
    },
    Grayscale,
    Invert,
    HueRotate {
        degrees: i32,
    },
}

/// Result returned after processing.
#[derive(Debug, Serialize)]
pub struct ImageResult {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub format: String,
}

/// Process an image through a pipeline of operations and save the result.
///
/// Operations are applied in the order they appear in the `operations` vec.
/// For JPEG output, `jpeg_quality` controls compression (1-100, default 90).
#[tauri::command]
pub async fn process_image(
    input_path: String,
    output_path: String,
    operations: Vec<ImageOperation>,
    jpeg_quality: Option<u8>,
) -> Result<ImageResult, String> {
    validate_path(&input_path)?;
    validate_path(&output_path)?;

    // Size guard: refuse files over 100 MB
    let input_meta = tokio::fs::metadata(&input_path)
        .await
        .map_err(|e| format!("Cannot read file: {e}"))?;
    if input_meta.len() > 100 * 1024 * 1024 {
        return Err("Image exceeds 100 MB limit".to_string());
    }

    // Load image (blocking — spawn on rayon / blocking thread)
    let input = input_path.clone();
    let output = output_path.clone();
    let quality = jpeg_quality.unwrap_or(90).clamp(1, 100);

    let result = tokio::task::spawn_blocking(move || -> Result<ImageResult, String> {
        let mut img: DynamicImage =
            image::open(&input).map_err(|e| format!("Failed to open image: {e}"))?;

        // Apply operations in order
        for op in &operations {
            img = apply_operation(img, op)?;
        }

        let (width, height) = img.dimensions();

        // Determine output format from extension
        let ext = std::path::Path::new(&output)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_lowercase();

        let format_name = ext.clone();

        // Save with appropriate format
        match ext.as_str() {
            "jpg" | "jpeg" => {
                // JPEG: no alpha channel — convert to RGB8
                let rgb = img.to_rgb8();
                let file = std::fs::File::create(&output)
                    .map_err(|e| format!("Failed to create output file: {e}"))?;
                let writer = BufWriter::new(file);
                let encoder = JpegEncoder::new_with_quality(writer, quality);
                rgb.write_with_encoder(encoder)
                    .map_err(|e| format!("Failed to encode JPEG: {e}"))?;
            }
            _ => {
                // All other formats: auto-detect from extension
                img.save(&output)
                    .map_err(|e| format!("Failed to save image: {e}"))?;
            }
        }

        // Get output file size
        let output_meta =
            std::fs::metadata(&output).map_err(|e| format!("Failed to read output: {e}"))?;

        Ok(ImageResult {
            path: output,
            width,
            height,
            size: output_meta.len(),
            format: format_name,
        })
    })
    .await
    .map_err(|e| format!("Processing thread failed: {e}"))??;

    Ok(result)
}

/// Apply a single operation to a DynamicImage, returning the modified image.
fn apply_operation(img: DynamicImage, op: &ImageOperation) -> Result<DynamicImage, String> {
    match op {
        ImageOperation::Crop {
            x,
            y,
            width,
            height,
        } => {
            if *width == 0 || *height == 0 {
                return Err("Crop dimensions must be non-zero".to_string());
            }
            let (img_w, img_h) = img.dimensions();
            if x + width > img_w || y + height > img_h {
                return Err(format!(
                    "Crop area ({},{} {}x{}) exceeds image bounds ({}x{})",
                    x, y, width, height, img_w, img_h
                ));
            }
            Ok(img.crop_imm(*x, *y, *width, *height))
        }
        ImageOperation::Resize { width, height } => {
            const MAX_DIMENSION: u32 = 16384;
            const MAX_PIXELS: u64 = 256_000_000; // 256 megapixels

            if *width == 0 || *height == 0 {
                return Err("Resize dimensions must be non-zero".to_string());
            }
            if *width > MAX_DIMENSION || *height > MAX_DIMENSION {
                return Err(format!(
                    "Resize dimension {}x{} exceeds maximum allowed ({}x{})",
                    width, height, MAX_DIMENSION, MAX_DIMENSION
                ));
            }
            let total_pixels = *width as u64 * *height as u64;
            if total_pixels > MAX_PIXELS {
                return Err(format!(
                    "Resize would produce {} megapixels, exceeding the {} MP limit",
                    total_pixels / 1_000_000,
                    MAX_PIXELS / 1_000_000
                ));
            }
            Ok(img.resize_exact(*width, *height, FilterType::Lanczos3))
        }
        ImageOperation::Rotate90 => Ok(img.rotate90()),
        ImageOperation::Rotate180 => Ok(img.rotate180()),
        ImageOperation::Rotate270 => Ok(img.rotate270()),
        ImageOperation::FlipH => Ok(img.fliph()),
        ImageOperation::FlipV => Ok(img.flipv()),
        ImageOperation::Brightness { value } => {
            Ok(DynamicImage::ImageRgba8(image::imageops::brighten(
                &img,
                *value,
            )))
        }
        ImageOperation::Contrast { value } => {
            Ok(DynamicImage::ImageRgba8(image::imageops::contrast(
                &img, *value,
            )))
        }
        ImageOperation::Blur { sigma } => Ok(img.blur(*sigma)),
        ImageOperation::Sharpen { sigma } => Ok(img.unsharpen(*sigma, 5)),
        ImageOperation::Grayscale => Ok(img.grayscale()),
        ImageOperation::Invert => {
            let mut inverted = img;
            inverted.invert();
            Ok(inverted)
        }
        ImageOperation::HueRotate { degrees } => {
            Ok(DynamicImage::ImageRgba8(image::imageops::huerotate(
                &img, *degrees,
            )))
        }
    }
}

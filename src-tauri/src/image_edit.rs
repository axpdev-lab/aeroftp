// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// ─── AeroImage: Image editing pipeline ──────────────────────────────────────
//
// Provides a single `process_image` command that accepts a pipeline of
// operations (crop, resize, rotate, flip, color adjustments, filters)
// and saves the result to the specified output path and format.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use serde::{Deserialize, Serialize};
use std::io::{BufWriter, Write};
use std::path::Path;

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

    // Load image (blocking: spawn on rayon / blocking thread)
    let input = input_path.clone();
    let output = output_path.clone();
    let quality = jpeg_quality.unwrap_or(90).clamp(1, 100);

    let result = tokio::task::spawn_blocking(move || -> Result<ImageResult, String> {
        // Cap the decode dimensions up front: a tiny file can declare enormous
        // dimensions (e.g. a 4 KB PNG claiming 60000x60000), and `image::open`
        // leans solely on the crate's default 512 MiB alloc ceiling. Bound width
        // and height to the same 16384 the resize path enforces so an oversized
        // image is rejected before allocation rather than spiking memory.
        // (CLAUDE-AV-B1-08)
        const MAX_DECODE_DIMENSION: u32 = 16384;
        let reader = image::ImageReader::open(&input)
            .map_err(|e| format!("Failed to open image: {e}"))?
            .with_guessed_format()
            .map_err(|e| format!("Failed to read image format: {e}"))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DECODE_DIMENSION);
        limits.max_image_height = Some(MAX_DECODE_DIMENSION);
        let mut img = decode_upright(reader, limits)?;

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
        let output_format = image::ImageFormat::from_extension(&ext)
            .ok_or_else(|| format!("Unsupported output image format: {ext}"))?;

        // Each save owns an exclusive staging file in the destination directory.
        // Persist only after encoding and flushing; failure drops just this file.
        let staged = stage_image(&img, Path::new(&output), output_format, quality)?;
        staged
            .persist(&output)
            .map_err(|e| format!("Failed to finalize output file: {e}"))?;

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

/// Encode and flush an image into an exclusively created, per-save staging file.
/// The caller publishes it atomically; dropping it cleans up an abandoned save.
fn stage_image(
    img: &DynamicImage,
    output: &Path,
    format: image::ImageFormat,
    quality: u8,
) -> Result<tempfile::NamedTempFile, String> {
    let parent = output.parent().unwrap_or(Path::new("."));
    let mut staged = tempfile::Builder::new()
        .prefix(".aeroftp-img-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(|e| format!("Failed to create output file: {e}"))?;
    {
        // Both encoders use the owned file handle; reopening a staging path can
        // truncate another save's file or follow a substituted path.
        let mut writer = BufWriter::new(staged.as_file_mut());
        if format == image::ImageFormat::Jpeg {
            let rgb = img.to_rgb8();
            let encoder = JpegEncoder::new_with_quality(&mut writer, quality);
            rgb.write_with_encoder(encoder)
                .map_err(|e| format!("Failed to encode JPEG: {e}"))?;
        } else {
            img.write_to(&mut writer, format)
                .map_err(|e| format!("Failed to save image: {e}"))?;
        }
        writer
            .flush()
            .map_err(|e| format!("Failed to flush output file: {e}"))?;
    }
    Ok(staged)
}

/// Apply a single operation to a DynamicImage, returning the modified image.
/// Decodes the picture the way the preview shows it: turned by its EXIF
/// orientation. A phone photo is stored sideways with an orientation tag, the
/// preview (and the crop drawn on it) follows the tag, and a decode that
/// ignores it crops the sideways picture: the selection lands elsewhere or
/// "exceeds image bounds" (#1075). The saved file carries no EXIF, so the
/// turn has to be in the pixels.
///
/// `limits` bound the decode as `ImageReader::decode` does, including the
/// decoded-buffer allocation (`max_alloc`, 512 MiB by default): the decoder
/// path does not reserve it on its own, so a small file declaring a huge
/// picture would otherwise be allocated before any check. A quarter turn
/// (orientations 5 to 8) builds the turned picture while the decoded one is
/// still alive, so it reserves a second buffer of the same size; a half turn
/// or a mirror works in place.
fn decode_upright<R: std::io::BufRead + std::io::Seek>(
    mut reader: image::ImageReader<R>,
    mut limits: image::Limits,
) -> Result<DynamicImage, String> {
    use image::ImageDecoder;
    reader.limits(limits.clone());
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Failed to open image: {e}"))?;
    use image::metadata::Orientation;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let buffers = match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => 2,
        _ => 1,
    };
    limits
        .reserve(decoder.total_bytes().saturating_mul(buffers))
        .map_err(|e| format!("Failed to open image: {e}"))?;
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("Failed to open image: {e}"))?;
    img.apply_orientation(orientation);
    Ok(img)
}

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
        ImageOperation::Brightness { value } => Ok(DynamicImage::ImageRgba8(
            image::imageops::brighten(&img, *value),
        )),
        ImageOperation::Contrast { value } => Ok(DynamicImage::ImageRgba8(
            image::imageops::contrast(&img, *value),
        )),
        ImageOperation::Blur { sigma } => Ok(img.blur(*sigma)),
        ImageOperation::Sharpen { sigma } => Ok(img.unsharpen(*sigma, 5)),
        ImageOperation::Grayscale => Ok(img.grayscale()),
        ImageOperation::Invert => {
            let mut inverted = img;
            inverted.invert();
            Ok(inverted)
        }
        ImageOperation::HueRotate { degrees } => Ok(DynamicImage::ImageRgba8(
            image::imageops::huerotate(&img, *degrees),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;

    #[test]
    fn overlapping_stages_keep_their_own_images_and_leave_existing_files_untouched() {
        let dir = tempfile::tempdir().expect("test directory");
        let existing = dir
            .path()
            .join(format!(".aeroftp-img-{}.tmp", std::process::id()));
        std::fs::write(&existing, b"existing file").expect("existing staging name");
        let mut pending = Vec::new();
        for (ext, format, width, color) in [
            ("jpg", image::ImageFormat::Jpeg, 8, [220, 30, 10, 255]),
            ("png", image::ImageFormat::Png, 12, [10, 220, 30, 100]),
            ("webp", image::ImageFormat::WebP, 16, [30, 10, 220, 200]),
        ] {
            let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                width,
                6,
                image::Rgba(color),
            ));
            let output = dir.path().join(format!("output.{ext}"));
            let staged = stage_image(&img, &output, format, 85).expect("stage image");
            let bytes = std::fs::read(staged.path()).expect("staged bytes");
            pending.push((staged, output, format, width, color, bytes));
        }
        for (index, (staged, _, _, _, _, bytes)) in pending.iter().enumerate() {
            assert_eq!(
                &std::fs::read(staged.path()).expect("stage survives other saves"),
                bytes
            );
            assert!(pending
                .iter()
                .skip(index + 1)
                .all(|other| other.0.path() != staged.path()));
        }
        // Finish in the opposite order to creation, after every stage exists.
        for (staged, output, format, width, color, bytes) in pending.into_iter().rev() {
            staged.persist(&output).expect("publish this save");
            assert_eq!(std::fs::read(&output).expect("published bytes"), bytes);
            let saved = image::open(&output).expect("decode saved image");
            assert_eq!(saved.dimensions(), (width, 6));
            if format != image::ImageFormat::Jpeg {
                assert_eq!(saved.get_pixel(0, 0), image::Rgba(color));
            }
        }
        assert_eq!(
            std::fs::read(&existing).expect("existing file survives"),
            b"existing file"
        );
        assert_eq!(std::fs::read_dir(dir.path()).expect("outputs").count(), 4);
    }

    #[tokio::test]
    async fn failed_publication_cleans_up_only_its_staging_file() {
        let dir = tempfile::tempdir().expect("test directory");
        let input = dir.path().join("original.png");
        DynamicImage::new_rgb8(8, 6).save(&input).expect("fixture");
        let original_bytes = std::fs::read(&input).expect("original bytes");
        let output = dir.path().join("destination.png");
        std::fs::create_dir(&output).expect("block publication with a directory");
        let marker = output.join("keep.txt");
        std::fs::write(&marker, b"keep").expect("destination contents");
        let other_stage = stage_image(
            &DynamicImage::new_rgb8(2, 3),
            &input,
            image::ImageFormat::Png,
            90,
        )
        .expect("another pending save");
        let other_bytes = std::fs::read(other_stage.path()).expect("other stage bytes");
        let err = process_image(
            input.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
            Vec::new(),
            None,
        )
        .await
        .expect_err("cannot replace a directory with an image");
        assert!(err.contains("Failed to finalize output file"), "{err}");
        assert_eq!(
            std::fs::read(&input).expect("original survives"),
            original_bytes
        );
        assert_eq!(
            std::fs::read(&marker).expect("destination survives"),
            b"keep"
        );
        assert_eq!(
            std::fs::read(other_stage.path()).expect("other stage survives"),
            other_bytes
        );
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("only owned stage remains")
                .count(),
            3
        );
        drop(other_stage);
        assert_eq!(
            std::fs::read_dir(dir.path())
                .expect("abandoned stage removed")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn saves_a_crop_as_a_copy_and_over_the_original_in_every_output_format() {
        let dir = tempfile::tempdir().expect("test directory");
        let original = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(8, 6, |x, y| {
            image::Rgba([(x * 30) as u8, (y * 40) as u8, 80, 255])
        }));
        for (ext, format) in [
            ("png", image::ImageFormat::Png),
            ("jpg", image::ImageFormat::Jpeg),
            ("jpeg", image::ImageFormat::Jpeg),
            ("webp", image::ImageFormat::WebP),
            ("bmp", image::ImageFormat::Bmp),
            ("tiff", image::ImageFormat::Tiff),
            ("gif", image::ImageFormat::Gif),
        ] {
            let input = dir.path().join(format!("original.{ext}"));
            // JPEG cannot encode an alpha channel, as in the production path.
            original
                .to_rgb8()
                .save_with_format(&input, format)
                .expect("fixture");
            let original_bytes = std::fs::read(&input).expect("original bytes");
            for replace in [false, true] {
                let output = if replace {
                    input.clone()
                } else {
                    dir.path().join(format!("copy.{ext}"))
                };
                let result = process_image(
                    input.to_string_lossy().into_owned(),
                    output.to_string_lossy().into_owned(),
                    vec![ImageOperation::Crop {
                        x: 2,
                        y: 1,
                        width: 4,
                        height: 3,
                    }],
                    Some(85),
                )
                .await
                .unwrap_or_else(|err| panic!("{ext}, replace={replace}: {err}"));
                assert_eq!(result.path, output.to_string_lossy());
                assert_eq!((result.width, result.height), (4, 3));
                assert_eq!(result.format, ext);
                let bytes = std::fs::read(&output).expect("saved bytes");
                assert_eq!(result.size, bytes.len() as u64);
                assert_eq!(image::guess_format(&bytes).expect("encoded format"), format);
                assert_eq!(
                    image::open(&output)
                        .expect("decode saved crop")
                        .dimensions(),
                    (4, 3)
                );
                if !replace {
                    assert_eq!(
                        std::fs::read(&input).expect("original survives"),
                        original_bytes
                    );
                }
            }
        }
        assert_eq!(std::fs::read_dir(dir.path()).expect("outputs").count(), 14);
    }

    /// A 4x2 JPEG whose EXIF says "turn 90 degrees clockwise to view".
    fn sideways_jpeg() -> Vec<u8> {
        jpeg_with_orientation(6)
    }

    /// A 4x2 JPEG carrying the EXIF orientation `value`.
    fn jpeg_with_orientation(value: u8) -> Vec<u8> {
        let pixels = image::RgbImage::from_pixel(4, 2, image::Rgb([200, 30, 30]));
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
        // TIFF header, one IFD entry: Orientation (0x0112), SHORT, 1, value.
        let exif: Vec<u8> = vec![
            b'I', b'I', 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00,
            0x01, 0x00, 0x00, 0x00, value, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        enc.set_exif_metadata(exif).expect("jpeg takes exif");
        enc.write_image(pixels.as_raw(), 4, 2, image::ExtendedColorType::Rgb8)
            .expect("encode");
        out
    }

    #[test]
    fn decodes_a_tagged_photo_the_way_the_preview_shows_it() {
        let bytes = sideways_jpeg();
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .expect("format");
        let img = decode_upright(reader, image::Limits::default()).expect("decode");
        // Stored 4x2, shown (and cropped) as 2x4.
        assert_eq!(img.dimensions(), (2, 4));
        // A crop of the bottom half of what the preview shows fits.
        let op = ImageOperation::Crop {
            x: 0,
            y: 2,
            width: 2,
            height: 2,
        };
        let cropped = apply_operation(img, &op).expect("crop inside the shown picture");
        assert_eq!(cropped.dimensions(), (2, 2));
    }

    #[test]
    fn decodes_an_untagged_picture_unchanged() {
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&[0u8; 4 * 2 * 3], 4, 2, image::ExtendedColorType::Rgb8)
            .expect("encode");
        let reader = image::ImageReader::new(std::io::Cursor::new(out))
            .with_guessed_format()
            .expect("format");
        assert_eq!(
            decode_upright(reader, image::Limits::default())
                .expect("decode")
                .dimensions(),
            (4, 2)
        );
    }

    #[test]
    fn refuses_a_picture_larger_than_the_allocation_limit_before_decoding_it() {
        // 4x2 RGB needs 24 bytes; a 16-byte allowance must refuse it, as
        // ImageReader::decode does with its 512 MiB default.
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&[0u8; 4 * 2 * 3], 4, 2, image::ExtendedColorType::Rgb8)
            .expect("encode");
        let reader = image::ImageReader::new(std::io::Cursor::new(out))
            .with_guessed_format()
            .expect("format");
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(16);
        assert!(decode_upright(reader, limits).is_err());
    }

    #[test]
    fn a_quarter_turn_reserves_the_turned_copy_too() {
        // 4x2 RGB is 24 bytes. A quarter turn holds the decoded and the turned
        // picture at once (48 bytes); a half turn works in place (24 bytes).
        let decode = |orientation: u8, max_alloc: u64| {
            let reader =
                image::ImageReader::new(std::io::Cursor::new(jpeg_with_orientation(orientation)))
                    .with_guessed_format()
                    .expect("format");
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(max_alloc);
            decode_upright(reader, limits)
        };
        assert!(decode(6, 40).is_err(), "quarter turn over the allowance");
        assert!(decode(6, 48).is_ok(), "quarter turn within it");
        assert!(decode(3, 40).is_ok(), "half turn needs one buffer");
    }
}

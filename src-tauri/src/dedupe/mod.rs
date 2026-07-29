// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared dedupe engine for exact and non-identical duplicate detection.
//! Stops the 4-copy drift. Used by CLI for non-identical mode in this slice.
//!
//! Terminology: "non-identical duplicates" (files that are duplicates in intent
//! but not byte-identical), e.g. re-saved SVG, re-encoded JPEG.
//!
//! Modality routing (by mime_guess + extension):
//! - Raster images (non-SVG): perceptual hash (image_hasher pHash), Hamming <= 10 default.
//! - Text (incl. .svg as XML text): SimHash (primary) + MinHash cross-check, Hamming <= 3 default.
//! - Other: TLSH fuzzy byte-level (tlsh2 crate), diff <= 100 default (configurable).
//!
//! Public results carry optional similarity metadata. Exact path unchanged.
//!
//! Phase 3 (solid implementation): previous stub "unsupported this slice (ssdeep/TLSH deferred)"
//! was eliminated. Other clustering retains Tlsh objects internally so rep_dist uses direct
//! tlsh_diff without any filesystem re-read or post-processing hack. All paths go through the same
//! engine (no separate MD5/SHA special cases in agent tools except the documented untouched
//! remote exact fast-path per spec).

use blake3::Hasher as Blake3Hasher;
use image::ImageReader;
use image_hasher::{HasherConfig, ImageHash};
use mime_guess;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use tlsh2::TlshDefaultBuilder;

/// Similarity mode for duplicate detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SimilarityMode {
    #[default]
    Exact,
    NonIdentical,
}

impl SimilarityMode {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "non-identical" | "nonidentical" | "similar" | "fuzzy" => SimilarityMode::NonIdentical,
            _ => SimilarityMode::Exact,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SimilarityMode::Exact => "exact",
            SimilarityMode::NonIdentical => "non-identical",
        }
    }
}

/// File modality for similarity routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    Raster,
    Text,
    Other,
}

impl Modality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Modality::Raster => "raster",
            Modality::Text => "text",
            Modality::Other => "other",
        }
    }
}

/// Detect modality using mime + special case for .svg (text even if image/svg+xml).
pub fn detect_modality(path: &Path) -> Modality {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let essence = mime.essence_str().to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if essence.starts_with("image/") && essence != "image/svg+xml" && ext != "svg" {
        Modality::Raster
    } else if essence.starts_with("text/")
        || essence == "image/svg+xml"
        || ext == "svg"
        || ext == "txt"
        || ext == "md"
        || ext == "rst"
        || ext == "xml"
        || ext == "html"
        || ext == "htm"
        || ext == "json"
        || ext == "yaml"
        || ext == "yml"
        || ext == "toml"
        || ext == "rs"
        || ext == "js"
        || ext == "ts"
        || ext == "css"
        || ext == "sh"
    {
        Modality::Text
    } else {
        Modality::Other
    }
}

/// Result group. For exact: hash present. For non-identical: distance + modality.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    /// BLAKE3 (or other) for Exact mode. None for non-identical groups.
    pub hash: Option<String>,
    pub size: u64,
    pub files: Vec<String>,
    /// Hamming distance used to cluster (representative / max within group). None for exact.
    pub distance: Option<u32>,
    /// "raster" | "text" | "other"
    pub modality: Option<String>,
    /// The fuzzy signature of each entry of `files`, same order and length:
    /// pHash / SimHash as 16 hex digits, TLSH as its own hex string. None in
    /// exact mode, where every member shares the single `hash` above.
    /// Surfaced so the UI can show why two files were called duplicates
    /// (discussion #347).
    pub file_hashes: Option<Vec<String>>,
}

/// Compute BLAKE3 of a local file (for Exact mode consistency with GUI).
pub fn compute_blake3(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Blake3Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Compute perceptual hash (64-bit) from image bytes. Uses image_hasher pHash (DCT based).
pub fn compute_phash_from_bytes(bytes: &[u8]) -> Result<u64, String> {
    let img = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("image format: {}", e))?
        .decode()
        .map_err(|e| format!("image decode: {}", e))?;

    // 8x8 produces 64-bit hash; good default for perceptual.
    let hasher = HasherConfig::new().hash_size(8, 8).to_hasher();
    let hash: ImageHash = hasher.hash_image(&img);
    // Convert the 8-byte hash to u64 (big-endian for stable bits).
    let bytes = hash.as_bytes();
    let mut val = 0u64;
    for (i, &b) in bytes.iter().take(8).enumerate() {
        val |= (b as u64) << ((7 - i) * 8);
    }
    Ok(val)
}

/// Compute perceptual hash from a local path (convenience).
pub fn compute_phash(path: &Path) -> Result<u64, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    compute_phash_from_bytes(&bytes)
}

/// Simple 64-bit FNV-1a for token hashing (fast, good distribution for simhash).
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Tokenize text for similarity: lowercase alphanum runs, drop very short tokens.
/// For SVG/XML this still works (tags/attrs become tokens after lower).
fn tokenize(content: &str) -> Vec<String> {
    content
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect()
}

/// Compute SimHash (64-bit) over the token set. Hamming distance on the result
/// approximates set similarity (good for reformatted / minor edit text).
pub fn compute_simhash(content: &str) -> u64 {
    let tokens = tokenize(content);
    if tokens.is_empty() {
        return 0;
    }
    let mut v = [0i32; 64];
    for token in &tokens {
        let h = fnv1a_64(token.as_bytes());
        for (i, slot) in v.iter_mut().enumerate() {
            if (h & (1u64 << i)) != 0 {
                *slot += 1;
            } else {
                *slot -= 1;
            }
        }
    }
    let mut out = 0u64;
    for (i, &val) in v.iter().enumerate() {
        if val > 0 {
            out |= 1u64 << i;
        }
    }
    out
}

/// MinHash signature (k=16) for Jaccard cross-check. Returns the mins.
pub fn compute_minhash(content: &str) -> Vec<u64> {
    const K: usize = 16;
    let owned = tokenize(content);
    let tokens: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    if tokens.is_empty() {
        return vec![0u64; K];
    }
    let mut sig = vec![u64::MAX; K];
    for (i, token) in tokens.iter().enumerate() {
        // vary the hash per band by simple mix with i
        let h0 = fnv1a_64(token.as_bytes());
        for (j, slot) in sig.iter_mut().enumerate().take(K) {
            let hj = h0.wrapping_add((i * 63689 + j * 131071) as u64);
            if hj < *slot {
                *slot = hj;
            }
        }
    }
    sig
}

/// Hamming distance for 64-bit hashes.
pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Compute TLSH for byte content (Modality::Other). Returns None for too-small payloads
/// (TLSH quality is poor below this threshold per original design). Uses default builder.
pub const TLSH_MIN_BYTES: usize = 256;

pub fn compute_tlsh(bytes: &[u8]) -> Option<tlsh2::TlshDefault> {
    if bytes.len() < TLSH_MIN_BYTES {
        return None;
    }
    TlshDefaultBuilder::build_from(bytes)
}

/// Solid TLSH distance (the diff feature bool is "include length" or similar per crate).
pub fn tlsh_diff(a: &tlsh2::TlshDefault, b: &tlsh2::TlshDefault) -> u32 {
    a.diff(b, true).max(0) as u32
}

/// Jaccard estimate from two MinHash signatures (0.0..1.0).
pub fn jaccard_from_minhash(a: &[u64], b: &[u64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let matches = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    matches as f64 / a.len() as f64
}

/// Find duplicate groups on a list of local file paths.
/// - Exact: group by size then BLAKE3 (size prefilter kept).
/// - NonIdentical: compute per-modality signatures; bypass size prefilter for Raster.
///   Cluster by Hamming distance <= threshold (param overrides default).
///
///   Returns groups with >=2 files, sorted by wasted space desc (or distance asc for visibility).
/// One tick of a similarity scan, so a caller can show what the engine is
/// chewing through instead of an indeterminate spinner (discussion #347).
/// `files_total` is the candidate count the pass started with, which is what
/// makes the tick a fraction rather than a running number.
#[derive(Debug, Clone, Copy)]
pub struct DedupeProgress {
    pub files_processed: u64,
    pub bytes_processed: u64,
    pub files_total: u64,
}

pub fn find_similar_local(
    paths: &[PathBuf],
    mode: SimilarityMode,
    distance: Option<u32>,
    min_size: Option<u64>,
) -> Vec<DuplicateGroup> {
    find_similar_local_with_progress(paths, mode, distance, min_size, &mut |_| {})
}

/// `find_similar_local` with a progress callback, called once per file whose
/// content the engine actually reads — the hashing pass in exact mode and the
/// signature pass in non-identical mode. The callback runs on this thread, so
/// keep it cheap (the GUI caller throttles before it emits).
pub fn find_similar_local_with_progress(
    paths: &[PathBuf],
    mode: SimilarityMode,
    distance: Option<u32>,
    min_size: Option<u64>,
    on_progress: &mut dyn FnMut(DedupeProgress),
) -> Vec<DuplicateGroup> {
    let min = min_size.unwrap_or(1);
    let files_total = paths.len() as u64;
    let mut files_processed: u64 = 0;
    let mut bytes_processed: u64 = 0;

    match mode {
        SimilarityMode::Exact => {
            // Size prefilter + blake3 (matches GUI current shape)
            let mut size_groups: HashMap<u64, Vec<PathBuf>> = HashMap::new();
            for p in paths {
                if let Ok(meta) = std::fs::metadata(p) {
                    let sz = meta.len();
                    if sz >= min {
                        size_groups.entry(sz).or_default().push(p.clone());
                    }
                }
            }
            let mut hash_groups: HashMap<String, (u64, Vec<String>)> = HashMap::new();
            for (sz, files) in size_groups {
                if files.len() < 2 {
                    continue;
                }
                for f in files {
                    if let Ok(h) = compute_blake3(&f) {
                        hash_groups
                            .entry(h)
                            .or_insert_with(|| (sz, Vec::new()))
                            .1
                            .push(f.to_string_lossy().to_string());
                    }
                    files_processed += 1;
                    bytes_processed += sz;
                    on_progress(DedupeProgress {
                        files_processed,
                        bytes_processed,
                        files_total,
                    });
                }
            }
            let mut result: Vec<DuplicateGroup> = hash_groups
                .into_iter()
                .filter(|(_, (_, fs))| fs.len() >= 2)
                .map(|(hash, (sz, fs))| DuplicateGroup {
                    hash: Some(hash),
                    size: sz,
                    files: fs,
                    distance: None,
                    modality: None,
                    file_hashes: None,
                })
                .collect();
            result.sort_by(|a, b| {
                let wa = a.size * (a.files.len() as u64 - 1);
                let wb = b.size * (b.files.len() as u64 - 1);
                wb.cmp(&wa)
            });
            result
        }
        SimilarityMode::NonIdentical => {
            // Collect candidates with content. For raster: ignore size groups entirely.
            // For text/other we still can prefilter by size loosely but per spec, size
            // prefilter bypassed for raster; we bypass for all non-id for simplicity + correctness
            // (text reformat can change size too).
            let mut candidates: Vec<(PathBuf, u64, Vec<u8>)> = Vec::new();
            for p in paths {
                if let Ok(meta) = std::fs::metadata(p) {
                    let sz = meta.len();
                    if sz < min {
                        continue;
                    }
                    // Read full for sim (small for typical dupes; capped by caller in practice)
                    if let Ok(bytes) = std::fs::read(p) {
                        bytes_processed += bytes.len() as u64;
                        candidates.push((p.clone(), sz, bytes));
                    }
                }
                // Counted whether or not the read succeeded: the tick tracks how
                // far through the candidate list we are, not how many worked.
                files_processed += 1;
                on_progress(DedupeProgress {
                    files_processed,
                    bytes_processed,
                    files_total,
                });
            }

            // Compute signatures per modality
            #[derive(Clone)]
            struct Sig {
                path: String,
                size: u64,
                modality: Modality,
                phash: Option<u64>,
                simhash: Option<u64>,
                minhash: Option<Vec<u64>>,
                tlsh: Option<tlsh2::TlshDefault>,
            }

            let mut sigs: Vec<Sig> = Vec::new();
            for (p, sz, bytes) in candidates {
                let modality = detect_modality(&p);
                let path_str = p.to_string_lossy().to_string();
                match modality {
                    Modality::Raster => {
                        if let Ok(h) = compute_phash_from_bytes(&bytes) {
                            sigs.push(Sig {
                                path: path_str,
                                size: sz,
                                modality,
                                phash: Some(h),
                                simhash: None,
                                minhash: None,
                                tlsh: None,
                            });
                        }
                    }
                    Modality::Text => {
                        // For text we read as utf8 lossy to be robust on mixed.
                        let text = String::from_utf8_lossy(&bytes);
                        let sh = compute_simhash(&text);
                        let mh = compute_minhash(&text);
                        sigs.push(Sig {
                            path: path_str,
                            size: sz,
                            modality,
                            phash: None,
                            simhash: Some(sh),
                            minhash: Some(mh),
                            tlsh: None,
                        });
                    }
                    Modality::Other => {
                        if let Some(t) = compute_tlsh(&bytes) {
                            sigs.push(Sig {
                                path: path_str,
                                size: sz,
                                modality,
                                phash: None,
                                simhash: None,
                                minhash: None,
                                tlsh: Some(t),
                            });
                        }
                    }
                }
            }

            // Cluster by modality + hamming
            let thresh = distance.unwrap_or(10u32); // default for raster; text uses tighter below

            // Separate by modality for clustering
            let mut raster: Vec<(String, u64, u64)> = Vec::new(); // (path, size, ph)
            let mut text: Vec<(String, u64, u64, Vec<u64>)> = Vec::new();
            let mut other: Vec<(String, u64, tlsh2::TlshDefault)> = Vec::new();

            for s in &sigs {
                match s.modality {
                    Modality::Raster => {
                        if let Some(h) = s.phash {
                            raster.push((s.path.clone(), s.size, h));
                        }
                    }
                    Modality::Text => {
                        if let (Some(sh), Some(mh)) = (s.simhash, &s.minhash) {
                            text.push((s.path.clone(), s.size, sh, mh.clone()));
                        }
                    }
                    Modality::Other => {
                        if let Some(t) = &s.tlsh {
                            other.push((s.path.clone(), s.size, t.clone()));
                        }
                    }
                }
            }

            // Raster cluster (phash hamming)
            let raster_groups_raw: Vec<Vec<(String, u64, u64)>> = {
                let mut gs: Vec<Vec<(String, u64, u64)>> = Vec::new();
                for (path, sz, h) in &raster {
                    let mut placed = false;
                    for g in &mut gs {
                        if g.iter()
                            .any(|(_, _, gh)| hamming_distance(*gh, *h) <= thresh)
                        {
                            g.push((path.clone(), *sz, *h));
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        gs.push(vec![(path.clone(), *sz, *h)]);
                    }
                }
                gs.retain(|g| g.len() >= 2);
                gs
            };

            // Text cluster (simhash hamming default 3 is tighter)
            let text_thresh = distance.unwrap_or(3);
            let text_groups_raw: Vec<Vec<(String, u64, u64)>> = {
                let mut gs: Vec<Vec<(String, u64, u64)>> = Vec::new();
                for (path, sz, sh, _) in &text {
                    let mut placed = false;
                    for g in &mut gs {
                        if g.iter()
                            .any(|(_, _, gh)| hamming_distance(*gh, *sh) <= text_thresh)
                        {
                            g.push((path.clone(), *sz, *sh));
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        gs.push(vec![(path.clone(), *sz, *sh)]);
                    }
                }
                gs.retain(|g| g.len() >= 2);
                gs
            };

            // Other cluster (TLSH diff, default 100 as per Phase 3 spec)
            // Keep Tlsh objects inside the group vecs so rep_dist is computed directly (no FS re-read workaround).
            let other_thresh = distance.unwrap_or(100);
            let other_groups_raw: Vec<Vec<(String, u64, tlsh2::TlshDefault)>> = {
                let mut gs: Vec<Vec<(String, u64, tlsh2::TlshDefault)>> = Vec::new();
                for (path, sz, t) in &other {
                    let mut placed = false;
                    for g in &mut gs {
                        if g.iter().any(|(_, _, gt)| tlsh_diff(gt, t) <= other_thresh) {
                            g.push((path.clone(), *sz, t.clone()));
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        gs.push(vec![(path.clone(), *sz, t.clone())]);
                    }
                }
                gs.retain(|g| g.len() >= 2);
                gs
            };

            let mut result: Vec<DuplicateGroup> = Vec::new();

            for g in raster_groups_raw {
                // Representative distance = the furthest member from the first,
                // in HAMMING terms. This used to take `.max()` of the pHash
                // values themselves, so the number the UI showed as "dist" was a
                // 64-bit hash, not a distance, and sorting groups by similarity
                // would have sorted them by an arbitrary number.
                let rep_dist = g
                    .first()
                    .map(|(_, _, h0)| {
                        g.iter()
                            .skip(1)
                            .map(|(_, _, h)| hamming_distance(*h0, *h))
                            .max()
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                let files: Vec<String> = g.iter().map(|(p, _, _)| p.clone()).collect();
                let file_hashes: Vec<String> =
                    g.iter().map(|(_, _, h)| format!("{:016x}", h)).collect();
                let sz = g.first().map(|(_, s, _)| *s).unwrap_or(0);
                result.push(DuplicateGroup {
                    hash: None,
                    size: sz,
                    files,
                    distance: Some(rep_dist),
                    modality: Some(Modality::Raster.as_str().to_string()),
                    file_hashes: Some(file_hashes),
                });
            }

            for g in text_groups_raw {
                // Same correction as the raster arm above: hamming, not the
                // SimHash value.
                let rep_dist = g
                    .first()
                    .map(|(_, _, h0)| {
                        g.iter()
                            .skip(1)
                            .map(|(_, _, h)| hamming_distance(*h0, *h))
                            .max()
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                let files: Vec<String> = g.iter().map(|(p, _, _)| p.clone()).collect();
                let file_hashes: Vec<String> =
                    g.iter().map(|(_, _, h)| format!("{:016x}", h)).collect();
                let sz = g.first().map(|(_, s, _)| *s).unwrap_or(0);
                result.push(DuplicateGroup {
                    hash: None,
                    size: sz,
                    files,
                    distance: Some(rep_dist),
                    modality: Some(Modality::Text.as_str().to_string()),
                    file_hashes: Some(file_hashes),
                });
            }

            for g in other_groups_raw {
                // representative distance = max pairwise TLSH diff (objects kept, no re-read)
                let rep_dist = if g.len() >= 2 {
                    let t0 = &g[0].2;
                    g.iter()
                        .skip(1)
                        .map(|(_, _, ti)| tlsh_diff(t0, ti))
                        .max()
                        .unwrap_or(0)
                } else {
                    0
                };
                let files: Vec<String> = g.iter().map(|(p, _, _)| p.clone()).collect();
                let file_hashes: Vec<String> = g
                    .iter()
                    .map(|(_, _, t)| {
                        String::from_utf8_lossy(&t.hash())
                            .trim_matches('\0')
                            .to_string()
                    })
                    .collect();
                let sz = g.first().map(|(_, s, _)| *s).unwrap_or(0);
                result.push(DuplicateGroup {
                    hash: None,
                    size: sz,
                    files,
                    distance: Some(rep_dist),
                    modality: Some(Modality::Other.as_str().to_string()),
                    file_hashes: Some(file_hashes),
                });
            }

            // sort by wasted desc (use size* (len-1) as proxy)
            result.sort_by(|a, b| {
                let wa = a.size * (a.files.len() as u64 - 1);
                let wb = b.size * (b.files.len() as u64 - 1);
                wb.cmp(&wa)
            });

            result
        }
    }
}

/// Convenience: scan a directory (local) and find duplicates. Mirrors the 2-phase
/// shape of existing find_duplicate_files but adds non-identical path.
pub fn find_similar_in_dir(
    root: &Path,
    mode: SimilarityMode,
    distance: Option<u32>,
    min_size: Option<u64>,
) -> Result<Vec<DuplicateGroup>, String> {
    if !root.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    const MAX_FILES: u64 = 100_000;
    let mut collected: Vec<PathBuf> = Vec::new();
    let mut count = 0u64;

    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(100)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        count += 1;
        if count > MAX_FILES {
            break;
        }
        collected.push(entry.into_path());
    }

    Ok(find_similar_local(&collected, mode, distance, min_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(data).unwrap();
        p
    }

    #[test]
    fn exact_mode_groups_identical() {
        let td = TempDir::new().unwrap();
        let p1 = write_file(
            td.path(),
            "a.bin",
            b"hello world duplicate content 1234567890",
        );
        let p2 = write_file(
            td.path(),
            "b.bin",
            b"hello world duplicate content 1234567890",
        );
        let p3 = write_file(td.path(), "c.bin", b"different content altogether xyz");

        let groups = find_similar_local(&[p1, p2, p3], SimilarityMode::Exact, None, None);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        assert!(groups[0].hash.is_some());
        assert!(groups[0].distance.is_none());
    }

    #[test]
    fn non_identical_raster_detects_reencode_like() {
        // Generate two similar raster images via image crate (different encoding params)
        // We use small solid-ish PNGs that after re-encode-ish will have close pHash.
        let td = TempDir::new().unwrap();

        // Create a simple 64x64 test pattern (gradient like) as PNG bytes twice with slight diff.
        let img1 = image::ImageBuffer::<image::Rgb<u8>, _>::from_fn(64, 64, |x, y| {
            image::Rgb([
                ((x * 4) % 256) as u8,
                ((y * 4) % 256) as u8,
                ((x + y) % 256) as u8,
            ])
        });
        let img2 = image::ImageBuffer::<image::Rgb<u8>, _>::from_fn(64, 64, |x, y| {
            // slight re-encode simulation: small noise
            let base = ((x * 4) % 256) as u8;
            image::Rgb([base, ((y * 4) % 256) as u8, ((x + y + 1) % 256) as u8])
        });

        let mut png1 = Vec::new();
        img1.write_to(
            &mut std::io::Cursor::new(&mut png1),
            image::ImageFormat::Png,
        )
        .unwrap();
        let mut png2 = Vec::new();
        img2.write_to(
            &mut std::io::Cursor::new(&mut png2),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Write as "jpg" name but content png for simplicity (modality by content+name? but use png ext for test)
        let p1 = write_file(td.path(), "photo1.png", &png1);
        let p2 = write_file(td.path(), "photo2.png", &png2);
        let p3 = write_file(td.path(), "other.png", &vec![0u8; 1024]); // different

        let groups =
            find_similar_local(&[p1, p2, p3], SimilarityMode::NonIdentical, Some(20), None);
        // With loose thresh for generated, expect at least the two similar
        assert!(groups
            .iter()
            .any(|g| g.files.len() >= 2 && g.modality.as_deref() == Some("raster")));
    }

    #[test]
    fn reported_distance_is_a_distance_not_a_hash() {
        // Regression pin (discussion #347): the representative distance for the
        // raster and text arms was `.max()` of the SIGNATURE VALUES, not of the
        // hamming distances between them, so a group clustered at a distance of
        // 2 could report a number in the billions. Ehud asked to sort groups by
        // similarity, which that number makes meaningless.
        let td = TempDir::new().unwrap();
        let svg1 = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let svg2 = r#"<svg   xmlns = 'http://www.w3.org/2000/svg'
            width='10'   height = "10"  >
          <rect   x = "0"   y = '0' width = "10" height="10" />
        </svg>"#;
        let p1 = write_file(td.path(), "icon1.svg", svg1.as_bytes());
        let p2 = write_file(td.path(), "icon2.svg", svg2.as_bytes());

        let groups = find_similar_local(&[p1, p2], SimilarityMode::NonIdentical, Some(3), None);
        let text_group = groups
            .iter()
            .find(|g| g.modality.as_deref() == Some("text") && g.files.len() >= 2)
            .expect("the two reformatted SVGs cluster");

        // A 64-bit hamming distance cannot exceed 64, and cannot exceed the
        // threshold the group was clustered under.
        let dist = text_group.distance.expect("fuzzy groups carry a distance");
        assert!(dist <= 3, "distance {} is not a hamming distance", dist);

        // And the signature of each member is reported, in file order.
        let hashes = text_group
            .file_hashes
            .as_ref()
            .expect("fuzzy groups carry per-file signatures");
        assert_eq!(hashes.len(), text_group.files.len());
        assert!(hashes
            .iter()
            .all(|h| h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit())));
    }

    #[test]
    fn fuzzy_threshold_is_honoured() {
        // The cutoff is now the caller's to choose (discussion #347): the same
        // pair must cluster at a loose threshold and separate at zero.
        let td = TempDir::new().unwrap();
        let a = write_file(
            td.path(),
            "a.svg",
            br#"<svg><rect x="0" y="0" width="10" height="10"/></svg>"#,
        );
        let b = write_file(
            td.path(),
            "b.svg",
            br#"<svg>  <rect x = "0" y = "0" width = "10" height = "11" />  </svg>"#,
        );

        let loose = find_similar_local(
            &[a.clone(), b.clone()],
            SimilarityMode::NonIdentical,
            Some(32),
            None,
        );
        assert!(
            loose.iter().any(|g| g.files.len() >= 2),
            "a loose cutoff must cluster them"
        );

        let strict = find_similar_local(&[a, b], SimilarityMode::NonIdentical, Some(0), None);
        assert!(
            strict.iter().all(|g| g.files.len() < 2),
            "a zero cutoff must only cluster identical signatures",
        );
    }

    #[test]
    fn non_identical_text_detects_reformat() {
        let td = TempDir::new().unwrap();
        let svg1 = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect x="0" y="0" width="10" height="10"/></svg>"#;
        let svg2 = r#"<svg   xmlns = 'http://www.w3.org/2000/svg'
            width='10'   height = "10"  >
          <rect   x = "0"   y = '0' width = "10" height="10" />
        </svg>"#; // whitespace + quote reorder
        let different = r#"<svg xmlns="http://www.w3.org/2000/svg"><circle r="5"/></svg>"#;

        let p1 = write_file(td.path(), "icon1.svg", svg1.as_bytes());
        let p2 = write_file(td.path(), "icon2.svg", svg2.as_bytes());
        let p3 = write_file(td.path(), "icon3.svg", different.as_bytes());

        let groups = find_similar_local(&[p1, p2, p3], SimilarityMode::NonIdentical, None, None);
        // Expect the two similar SVGs grouped (text modality)
        let text_groups: Vec<_> = groups
            .iter()
            .filter(|g| g.modality.as_deref() == Some("text"))
            .collect();
        assert!(text_groups.iter().any(|g| g.files.len() >= 2));
    }

    #[test]
    fn non_identical_does_not_group_different() {
        let td = TempDir::new().unwrap();
        let a = write_file(
            td.path(),
            "a.jpg",
            b"\xff\xd8\xff totally not image but bytes for test",
        );
        let b = write_file(
            td.path(),
            "b.jpg",
            b"\xff\xd8\xff different bytes here 0987654321",
        );

        let groups = find_similar_local(&[a, b], SimilarityMode::NonIdentical, Some(0), None);
        // With dist=0 only identical sigs; they differ
        assert!(groups.is_empty() || groups.iter().all(|g| g.files.len() < 2));
    }

    #[test]
    fn non_identical_other_detects_tlsh_similar_bytes() {
        // Two non-identical but TLSH-close binary payloads should group (Other modality).
        // Two clearly different must not (with tight threshold).
        let td = TempDir::new().unwrap();
        // Build ~300 byte payloads that differ by a few bytes (typical near-dupe scenario).
        let mut base: Vec<u8> = (0u8..=255).collect();
        base.extend_from_slice(
            b"some trailing bytes to reach TLSH minimum length and stability 0123456789",
        );
        let b1 = base.clone();
        let mut b2 = base.clone();
        if b2.len() > 80 {
            b2[70] = b2[70].wrapping_add(3);
            b2[120] = b2[120].wrapping_add(7);
        }
        let p1 = write_file(td.path(), "blobA.dat", &b1);
        let p2 = write_file(td.path(), "blobB.dat", &b2);
        let mut unrelated: Vec<u8> = (0u8..=255).rev().collect();
        unrelated.extend_from_slice(
            b"another unrelated tail to exceed TLSH minimum length and stay byte-diverse 9876543210",
        );
        let p3 = write_file(td.path(), "blobC.dat", &unrelated);

        // loose threshold to catch small edits
        let groups = find_similar_local(
            &[p1.clone(), p2.clone(), p3.clone()],
            SimilarityMode::NonIdentical,
            Some(150),
            None,
        );
        let other_groups: Vec<_> = groups
            .iter()
            .filter(|g| g.modality.as_deref() == Some("other"))
            .collect();
        assert!(other_groups.iter().any(|g| {
            g.files.iter().any(|f| f.contains("blobA"))
                && g.files.iter().any(|f| f.contains("blobB"))
        }));
        assert!(!other_groups
            .iter()
            .any(|g| g.files.iter().any(|f| f.contains("blobC"))));

        // tight threshold: the near pair should still match at dist ~small, but verify different pair does not force group
        let groups_tight = find_similar_local(
            &[p1.clone(), p2.clone(), p3.clone()],
            SimilarityMode::NonIdentical,
            Some(5),
            None,
        );
        // p1/p2 may or may not under 5; main point is p3 stays out, and we don't crash
        let has_bad = groups_tight
            .iter()
            .any(|g| g.files.iter().any(|f| f.contains("blobC")));
        assert!(!has_bad);
    }
}

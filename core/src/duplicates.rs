// Duplicate detection.
//
// Two passes, both on demand only (never automatic):
//   1. exact_duplicates: group by file size, then streaming FNV-1a of the
//      full contents. Catches byte-identical copies of any file type.
//   2. near_duplicates: 64-bit dHash on a tiny decode (ideally the cached
//      thumbnail, so no original needs re-decoding), grouped by Hamming
//      distance <= 5. Catches re-saves / resizes of the same picture.

use rayon::prelude::*;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DupGroup {
    pub paths: Vec<String>,
    pub exact: bool,
}

fn hash_file(p: &str) -> Option<u64> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = [0u8; 1 << 16];
    let mut h: u64 = 0xcbf29ce484222325;
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for b in &buf[..n] {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    Some(h)
}

/// Byte-identical duplicates among `paths`.
pub fn exact_duplicates(paths: &[String]) -> Vec<DupGroup> {
    let mut by_size: HashMap<u64, Vec<&String>> = HashMap::new();
    for p in paths {
        if let Ok(m) = std::fs::metadata(p) {
            by_size.entry(m.len()).or_default().push(p);
        }
    }
    let candidates: Vec<&String> = by_size
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();
    let hashed: Vec<(u64, String)> = candidates
        .par_iter()
        .filter_map(|p| hash_file(p).map(|h| (h, (*p).clone())))
        .collect();
    let mut by_hash: HashMap<u64, Vec<String>> = HashMap::new();
    for (h, p) in hashed {
        by_hash.entry(h).or_default().push(p);
    }
    by_hash
        .into_values()
        .filter(|v| v.len() > 1)
        .map(|paths| DupGroup { paths, exact: true })
        .collect()
}

fn dhash(path: &Path) -> Option<u64> {
    let img = image::open(path).ok()?;
    let g = img
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h <<= 1;
            if g.get_pixel(x, y)[0] > g.get_pixel(x + 1, y)[0] {
                h |= 1;
            }
        }
    }
    Some(h)
}

/// Visually similar images. `pairs` maps each original path to the small
/// file to decode for hashing (its cached thumbnail when available).
pub fn near_duplicates(pairs: &[(String, String)]) -> Vec<DupGroup> {
    let hashed: Vec<(u64, &String)> = pairs
        .par_iter()
        .filter_map(|(orig, small)| dhash(Path::new(small)).map(|h| (h, orig)))
        .collect();

    // Greedy grouping by Hamming distance (folders are small enough for n²).
    let mut used = vec![false; hashed.len()];
    let mut groups = Vec::new();
    for i in 0..hashed.len() {
        if used[i] {
            continue;
        }
        let mut group = vec![hashed[i].1.clone()];
        for j in (i + 1)..hashed.len() {
            if !used[j] && (hashed[i].0 ^ hashed[j].0).count_ones() <= 5 {
                used[j] = true;
                group.push(hashed[j].1.clone());
            }
        }
        if group.len() > 1 {
            groups.push(DupGroup {
                paths: group,
                exact: false,
            });
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn finds_exact_and_near() {
        let tmp = tempfile::tempdir().unwrap();
        let mut img = RgbImage::new(200, 150);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x % 251) as u8, (y % 241) as u8, ((x * y) % 253) as u8]);
        }
        let a = tmp.path().join("a.png");
        let b = tmp.path().join("b.png");
        let c = tmp.path().join("c.jpg");
        img.save(&a).unwrap();
        std::fs::copy(&a, &b).unwrap(); // exact copy
        image::DynamicImage::ImageRgb8(img.clone())
            .resize(100, 75, image::imageops::FilterType::Triangle)
            .to_rgb8()
            .save(&c)
            .unwrap(); // resized re-save: near-duplicate

        let paths: Vec<String> = [&a, &b, &c]
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let exact = exact_duplicates(&paths);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].paths.len(), 2);

        let pairs: Vec<(String, String)> =
            paths.iter().map(|p| (p.clone(), p.clone())).collect();
        let near = near_duplicates(&pairs);
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].paths.len(), 3);
    }
}

// Thumbnail pipeline.
//
// - Images: decoded with the `image` crate, resized with a triangle filter.
// - RAW: embedded JPEG preview extracted (see raw.rs) then resized.
// - Exotic images (JP2, AVIF, HEIC, JXL, SVG, PSD) and videos: delegated
//   to the ffmpeg side-car (see video.rs); this module only handles the
//   pure-Rust path so it stays fully testable headless.
//
// Batches run on the rayon thread pool: on a Ryzen 5900X all 24 threads
// decode in parallel, which is where the "ACDSee feel" comes from.

use crate::cache::ThumbCache;
use crate::{fnv1a64, raw, EntryKind};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use rayon::prelude::*;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ThumbOut {
    pub src: String,
    pub thumb: String,
    pub w: u32,
    pub h: u32,
    pub ok: bool,
}

/// Ensure a thumbnail exists for one file. Returns the cached record if the
/// source is unchanged, otherwise decodes and stores a fresh thumbnail.
pub fn ensure_thumb(
    cache: &ThumbCache,
    src: &Path,
    kind: EntryKind,
    tsize: u32,
) -> Option<ThumbOut> {
    let meta = std::fs::metadata(src).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let size = meta.len();
    let src_str = src.to_string_lossy().to_string();

    if let Some(rec) = cache.get(&src_str, mtime, size, tsize) {
        return Some(ThumbOut {
            src: src_str,
            thumb: rec.thumb_path,
            w: rec.width,
            h: rec.height,
            ok: true,
        });
    }

    let img = decode(src, kind)?;
    let (w0, h0) = img.dimensions();
    let img = if w0 > tsize || h0 > tsize {
        img.resize(tsize, tsize, FilterType::Triangle)
    } else {
        img
    };
    let (w, h) = img.dimensions();

    let key = fnv1a64(format!("{}|{}|{}|{}", src_str, mtime, size, tsize).as_bytes());
    let has_alpha = img.color().has_alpha();
    let (ext, out): (&str, DynamicImage) = if has_alpha {
        ("png", img)
    } else {
        ("jpg", DynamicImage::ImageRgb8(img.to_rgb8()))
    };
    let thumb_path = cache.dir.join("thumbs").join(format!("{:016x}.{}", key, ext));
    out.save(&thumb_path).ok()?;

    let thumb_str = thumb_path.to_string_lossy().to_string();
    cache.put(&src_str, mtime, size, tsize, &thumb_str, w, h);
    Some(ThumbOut {
        src: src_str,
        thumb: thumb_str,
        w,
        h,
        ok: true,
    })
}

/// Decode an image or a RAW preview into a DynamicImage,
/// applying the EXIF orientation so portrait shots come out upright.
fn decode(src: &Path, kind: EntryKind) -> Option<DynamicImage> {
    let (img, orientation) = match kind {
        EntryKind::Raw => {
            let jpeg = raw::extract_preview(src)?;
            let o = exif_orientation_from_bytes(&jpeg);
            (image::load_from_memory(&jpeg).ok()?, o)
        }
        _ => (image::open(src).ok()?, exif_orientation(src)),
    };
    Some(apply_orientation(img, orientation))
}

fn exif_orientation(src: &Path) -> u32 {
    let Ok(file) = std::fs::File::open(src) else { return 1 };
    let mut reader = std::io::BufReader::new(file);
    exif::Reader::new()
        .read_from_container(&mut reader)
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1)
}

fn exif_orientation_from_bytes(data: &[u8]) -> u32 {
    let mut cur = std::io::Cursor::new(data);
    exif::Reader::new()
        .read_from_container(&mut cur)
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1)
}

fn apply_orientation(img: DynamicImage, o: u32) -> DynamicImage {
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Generate thumbnails for a batch in parallel. `emit` is called from worker
/// threads as each thumbnail becomes ready (or fails), so the UI can display
/// results progressively instead of waiting for the whole folder.
///
/// `cancelled` is consulted BEFORE each decode. This is what makes leaving a
/// folder immediate: without it the pool keeps decoding thousands of pictures
/// nobody will look at, and the next folder's batch waits its turn behind
/// them - thumbnails that never seem to come.
pub fn batch<F, C>(
    cache: &ThumbCache,
    items: &[(String, EntryKind)],
    tsize: u32,
    emit: F,
    cancelled: C,
) where
    F: Fn(ThumbOut) + Sync + Send,
    C: Fn() -> bool + Sync + Send,
{
    items.par_iter().for_each(|(path, kind)| {
        if cancelled() {
            return; // a newer folder is waiting: drop the rest of this batch
        }
        let out = ensure_thumb(cache, Path::new(path), *kind, tsize).unwrap_or(ThumbOut {
            src: path.clone(),
            thumb: String::new(),
            w: 0,
            h: 0,
            ok: false,
        });
        emit(out);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn make_jpeg(path: &Path, w: u32, h: u32) {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8]);
        }
        img.save(path).unwrap();
    }

    #[test]
    fn thumb_created_then_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let src = tmp.path().join("big.jpg");
        make_jpeg(&src, 1600, 900);

        let t1 = ensure_thumb(&cache, &src, EntryKind::Image, 384).unwrap();
        assert!(t1.ok);
        assert!(t1.w <= 384 && t1.h <= 384);
        assert!(Path::new(&t1.thumb).exists());

        // Second call must be a cache hit returning the same file.
        let t2 = ensure_thumb(&cache, &src, EntryKind::Image, 384).unwrap();
        assert_eq!(t1.thumb, t2.thumb);
    }

    #[test]
    fn png_alpha_kept_as_png() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let src = tmp.path().join("alpha.png");
        let img = image::RgbaImage::from_pixel(600, 400, image::Rgba([10, 20, 30, 128]));
        img.save(&src).unwrap();

        let t = ensure_thumb(&cache, &src, EntryKind::Image, 256).unwrap();
        assert!(t.thumb.ends_with(".png"));
    }

    /// A cancelled batch must stop decoding, not merely stop reporting.
    #[test]
    fn cancelling_a_batch_stops_the_work() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let mut items = Vec::new();
        for i in 0..60 {
            let p = tmp.path().join(format!("c{i}.jpg"));
            make_jpeg(&p, 1280, 720);
            items.push((p.to_string_lossy().to_string(), EntryKind::Image));
        }
        let seen = AtomicUsize::new(0);
        // Cancel as soon as a handful have gone through.
        batch(
            &cache,
            &items,
            384,
            |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
            || seen.load(Ordering::SeqCst) >= 4,
        );
        let n = seen.load(Ordering::SeqCst);
        assert!(n >= 4, "expected at least the first few, got {n}");
        assert!(n < items.len(), "the whole batch ran despite cancellation");
    }

    #[test]
    fn batch_parallel_100_images() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let mut items = Vec::new();
        for i in 0..100 {
            let p = tmp.path().join(format!("img{i}.jpg"));
            make_jpeg(&p, 1280, 720);
            items.push((p.to_string_lossy().to_string(), EntryKind::Image));
        }

        let done = AtomicUsize::new(0);
        let start = Instant::now();
        batch(
            &cache,
            &items,
            384,
            |out| {
                assert!(out.ok, "failed on {}", out.src);
                done.fetch_add(1, Ordering::Relaxed);
            },
            || false,
        );
        let cold = start.elapsed();
        assert_eq!(done.load(Ordering::Relaxed), 100);

        // Warm pass: everything must come from the cache, much faster.
        let start = Instant::now();
        batch(&cache, &items, 384, |_| {}, || false);
        let warm = start.elapsed();
        println!("cold: {:?}, warm: {:?}", cold, warm);
        assert!(warm < cold);
    }
}

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
use image::{DynamicImage, GenericImageView};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

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
        // `thumbnail` averages areas with integer arithmetic: several times
        // faster than a Triangle resample on a 30-100 Mpx picture, and just
        // as clean at thumbnail size, where the extra sharpness is invisible.
        img.thumbnail(tsize, tsize)
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
        _ => {
            let img = image::open(src).ok()?;
            // Only JPEG keeps its orientation in a header we can reach
            // cheaply. For PNG and TIFF the EXIF reader walks or loads the
            // WHOLE file - a second full read of every big picture, for a
            // tag those formats almost never carry.
            let o = if is_jpeg(src) { exif_orientation(src) } else { 1 };
            (img, o)
        }
    };
    Some(apply_orientation(img, orientation))
}

fn is_jpeg(src: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 2];
    std::fs::File::open(src)
        .and_then(|mut f| f.read_exact(&mut head))
        .map(|_| head == [0xFF, 0xD8])
        .unwrap_or(false)
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
    // Order matters as much as speed here. `par_iter` over the whole slice
    // splits it across the pool, so a thread starts in the middle of the
    // batch and thumbnails appear scattered instead of top to bottom.
    // Every worker takes the NEXT item of the list instead: work starts in
    // exactly the requested order (the UI puts what is on screen first),
    // and a picture that takes twenty seconds to decode holds one thread,
    // not the whole screen. (Waves of 2x the cores used to wait for their
    // slowest member before the next wave could start.)
    let next = AtomicUsize::new(0);
    let workers = rayon::current_num_threads().max(1);
    rayon::scope(|s| {
        for _ in 0..workers {
            s.spawn(|_| loop {
                if cancelled() {
                    return; // a newer folder is waiting: drop the rest
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some((path, kind)) = items.get(i) else { return };
                let failed = || ThumbOut {
                    src: path.clone(),
                    thumb: String::new(),
                    w: 0,
                    h: 0,
                    ok: false,
                };
                let _watch = crate::watchdog::Watch::start(path);
                // A decoder that panics on one strange file used to unwind
                // through the whole wave and end the request: every picture
                // after it stayed blank. Contain it to that one file.
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ensure_thumb(cache, Path::new(path), *kind, tsize)
                }))
                .unwrap_or_else(|_| {
                    crate::watchdog::log(&format!("decoder panicked on {path}"));
                    None
                })
                .unwrap_or_else(failed);
                emit(out);
            });
        }
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

    /// Thumbnails must come back roughly in the order they were asked for:
    /// the first screenful before anything below the fold. A plain par_iter
    /// over the whole slice fails this - it starts threads in the middle.
    #[test]
    fn results_follow_the_requested_order() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let mut items = Vec::new();
        for i in 0..200 {
            let p = tmp.path().join(format!("o{i:03}.jpg"));
            make_jpeg(&p, 900, 600);
            items.push((p.to_string_lossy().to_string(), EntryKind::Image));
        }
        let order = std::sync::Mutex::new(Vec::new());
        batch(&cache, &items, 256, |out| {
            let name = Path::new(&out.src).file_stem().unwrap().to_string_lossy().to_string();
            order.lock().unwrap().push(name[1..].parse::<usize>().unwrap());
        }, || false);

        let seen = order.lock().unwrap();
        assert_eq!(seen.len(), items.len());
        // Every one of the first fifty must be done before the last fifty
        // start: exact order inside a wavefront is up to the pool.
        let first_fifty_end = seen.iter().position(|&i| i >= 150).unwrap_or(seen.len());
        let early = seen[..first_fifty_end].iter().filter(|&&i| i < 50).count();
        assert_eq!(early, 50, "the top of the list must be finished first");
    }

    /// Reproduction of the reported stall: a folder full of BIG pictures is
    /// generating, the user opens another folder, and its thumbnails must
    /// start arriving promptly. Run explicitly:
    ///   cargo test --release --lib repro_switch_folder -- --ignored --nocapture
    #[test]
    #[ignore]
    fn repro_switch_folder() {
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let cache = Arc::new(ThumbCache::open(&tmp.path().join("cache")).unwrap());

        // Folder A: big pictures, the kind that take seconds each.
        let a_dir = tmp.path().join("A");
        std::fs::create_dir_all(&a_dir).unwrap();
        let mut a_items = Vec::new();
        let t_gen = Instant::now();
        for i in 0..96 {
            let p = a_dir.join(format!("big{i:03}.png"));
            let img = RgbImage::from_fn(7680, 4320, |x, y| {
                Rgb([(x / 30 % 255) as u8, (y / 17 % 255) as u8, ((x ^ y) % 255) as u8])
            });
            img.save(&p).unwrap();
            a_items.push((p.to_string_lossy().to_string(), EntryKind::Image));
        }
        // Folder B: ordinary pictures, never seen before.
        let b_dir = tmp.path().join("B");
        std::fs::create_dir_all(&b_dir).unwrap();
        let mut b_items = Vec::new();
        for i in 0..48 {
            let p = b_dir.join(format!("small{i:03}.jpg"));
            make_jpeg(&p, 1600, 1000);
            b_items.push((p.to_string_lossy().to_string(), EntryKind::Image));
        }
        println!("fixtures ready in {:?}", t_gen.elapsed());

        let gen = Arc::new(AtomicU64::new(1));
        let a_done = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();

        // Start folder A, exactly like request_thumbs does.
        let (c, g, d) = (cache.clone(), gen.clone(), a_done.clone());
        let a_thread = std::thread::spawn(move || {
            batch(&c, &a_items, 256, |_| { d.fetch_add(1, Ordering::SeqCst); },
                  || g.load(Ordering::SeqCst) != 1);
        });

        std::thread::sleep(std::time::Duration::from_millis(600));
        println!("A after 0.6 s: {} done", a_done.load(Ordering::SeqCst));

        // The user opens folder B.
        gen.store(2, Ordering::SeqCst);
        let switched = Instant::now();
        let first_b = std::sync::Mutex::new(None::<std::time::Duration>);
        let b_done = AtomicUsize::new(0);
        batch(&cache, &b_items, 256, |_| {
            let mut f = first_b.lock().unwrap();
            if f.is_none() { *f = Some(switched.elapsed()); }
            b_done.fetch_add(1, Ordering::SeqCst);
        }, || gen.load(Ordering::SeqCst) != 2);
        let all_b = switched.elapsed();

        a_thread.join().unwrap();
        println!("A stopped {:?} after the switch", switched.elapsed());
        println!("B first thumbnail after {:?}", first_b.lock().unwrap().unwrap());
        println!("B all {} thumbnails after {:?}", b_done.load(Ordering::SeqCst), all_b);
        println!("total {:?}", start.elapsed());
    }

    /// A cancelled batch must stop decoding, not merely stop reporting.
    /// One picture that takes long to decode must hold one thread, not the
    /// screen: with waves, nothing past its wave could start until it ended.
    #[test]
    fn a_slow_picture_does_not_hold_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ThumbCache::open(&tmp.path().join("cache")).unwrap();
        let n = rayon::current_num_threads().max(2) * 4;
        let mut items = Vec::new();
        for i in 0..n {
            let p = tmp.path().join(format!("s{i:03}.jpg"));
            make_jpeg(&p, 320, 200);
            items.push((p.to_string_lossy().to_string(), EntryKind::Image));
        }
        let order = std::sync::Mutex::new(Vec::new());
        batch(&cache, &items, 128, |out| {
            if out.src == items[0].0 {
                std::thread::sleep(std::time::Duration::from_millis(1500)); // the slow one
            }
            order.lock().unwrap().push(out.src);
        }, || false);
        let order = order.lock().unwrap();
        assert_eq!(order.len(), n);
        assert_eq!(order.last(), Some(&items[0].0), "the others waited for the slow one");
    }

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

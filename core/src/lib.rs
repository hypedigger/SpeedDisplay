// lumen-core: engine for the Lumen media viewer.
// Everything here is pure library code, unit-tested headless.

pub mod cache;
pub mod duplicates;
pub mod exifwrite;
pub mod xmp;
pub mod raw;
pub mod scan;
pub mod search;
pub mod thumbs;
pub mod video;

use serde::{Deserialize, Serialize};

/// Kind of a file system entry, as classified by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    Image,
    Raw,
    Video,
    Other,
}

/// A single directory entry returned to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Modification time as UNIX milliseconds.
    pub mtime: i64,
    pub ext: String,
    /// Hidden file (dotfile, or Windows hidden attribute).
    #[serde(default)]
    pub hidden: bool,
}

/// Image extensions decoded natively by the `image` crate.
pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "webp", "tif", "tiff",
    "ico", "tga", "qoi", "exr", "hdr", "pbm", "pgm", "ppm", "pnm",
    // Decoded through embedded preview or ffmpeg fallback:
    "jp2", "j2k", "jpf", "jpx", "avif", "heic", "heif", "jxl", "svg", "psd",
];

/// RAW extensions handled through embedded JPEG preview extraction.
pub const RAW_EXTS: &[&str] = &[
    "arw", "nef", "nrw", "cr2", "cr3", "dng", "raf", "orf", "rw2", "pef", "srw", "raw",
];

/// Video extensions handled through ffmpeg.
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "avi", "webm", "mov", "wmv", "mpg", "mpeg", "m2ts",
    "mts", "ts", "flv", "3gp", "ogv", "vob",
];

/// Classify a file name into an EntryKind based on its extension.
pub fn classify(name: &str) -> (EntryKind, String) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let kind = if IMAGE_EXTS.contains(&ext.as_str()) {
        EntryKind::Image
    } else if RAW_EXTS.contains(&ext.as_str()) {
        EntryKind::Raw
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        EntryKind::Video
    } else {
        EntryKind::Other
    };
    (kind, ext)
}

/// Save raw RGBA pixels as a PNG (used for extracted system icons).
pub fn save_rgba_png(w: u32, h: u32, rgba: Vec<u8>, path: &std::path::Path) -> bool {
    image::RgbaImage::from_raw(w, h, rgba)
        .map(|img| img.save(path).is_ok())
        .unwrap_or(false)
}

/// Fast, header-only pixel dimensions of an image file, already swapped
/// when the EXIF orientation is a quarter turn (so it matches what both
/// the browser and our thumbnail pipeline display).
pub fn image_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    let (w, h) = image::image_dimensions(path).ok()?;
    Some(match exifwrite::get_orientation(path) {
        5..=8 => (h, w),
        _ => (w, h),
    })
}

/// Human-readable EXIF summary as (tag, value) pairs.
pub fn exif_summary(path: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(file) = std::fs::File::open(path) else { return out };
    let mut r = std::io::BufReader::new(file);
    let Ok(data) = exif::Reader::new().read_from_container(&mut r) else { return out };
    let tags = [
        exif::Tag::Make,
        exif::Tag::Model,
        exif::Tag::LensModel,
        exif::Tag::DateTimeOriginal,
        exif::Tag::ExposureTime,
        exif::Tag::FNumber,
        exif::Tag::PhotographicSensitivity,
        exif::Tag::FocalLength,
        exif::Tag::ExposureBiasValue,
        exif::Tag::Flash,
        exif::Tag::WhiteBalance,
        exif::Tag::GPSLatitude,
        exif::Tag::GPSLongitude,
        exif::Tag::Software,
    ];
    for tag in tags {
        let field = data
            .get_field(tag, exif::In::PRIMARY)
            .or_else(|| data.get_field(tag, exif::In::THUMBNAIL));
        if let Some(f) = field {
            let v = f.display_value().with_unit(&data).to_string();
            if !v.is_empty() {
                out.push((tag.to_string(), v));
            }
        }
    }
    out
}

/// Stable 64-bit FNV-1a hash, used for cache file names.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_extensions() {
        assert_eq!(classify("photo.JPG").0, EntryKind::Image);
        assert_eq!(classify("shot.arw").0, EntryKind::Raw);
        assert_eq!(classify("clip.MKV").0, EntryKind::Video);
        assert_eq!(classify("notes.txt").0, EntryKind::Other);
        assert_eq!(classify("archive.tar.gz").1, "gz");
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a64(b"lumen"), fnv1a64(b"lumen"));
        assert_ne!(fnv1a64(b"a"), fnv1a64(b"b"));
    }
}

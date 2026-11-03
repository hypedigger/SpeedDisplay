// EXIF orientation writing.
//
// A quarter-turn is recorded by patching the 2-byte Orientation tag (0x0112)
// inside the EXIF IFD0 — the pixel data is never touched, so this is exactly
// as lossless as storing the angle in a database, but every other program
// (Explorer, Lightroom, browsers...) sees the rotation too.
//
// Works on JPEG (Orientation lives in the APP1/Exif segment) and on any
// TIFF-based container, which includes Sony ARW, Nikon NEF, Canon CR2 and
// Adobe DNG. Returns Ok(false) when the file carries no Orientation tag:
// inserting one would mean rewriting the whole structure, so callers fall
// back to the display-only rotation stored in the cache database.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

fn rd_u16(b: &[u8], le: bool) -> u16 {
    if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }
}
fn rd_u32(b: &[u8], le: bool) -> u32 {
    if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }
}

/// Absolute file offset of the Orientation value, plus the byte order.
fn find_orientation(path: &Path) -> Option<(u64, bool)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 4];
    f.read_exact(&mut head).ok()?;

    // Where does the TIFF header start?
    let tiff_base: u64 = if &head[0..2] == b"\xFF\xD8" {
        // JPEG: walk the segment chain looking for APP1 "Exif\0\0".
        let mut pos: u64 = 2;
        loop {
            f.seek(SeekFrom::Start(pos)).ok()?;
            let mut marker = [0u8; 4];
            f.read_exact(&mut marker).ok()?;
            if marker[0] != 0xFF {
                return None;
            }
            let len = u16::from_be_bytes([marker[2], marker[3]]) as u64;
            if marker[1] == 0xE1 {
                let mut sig = [0u8; 6];
                f.read_exact(&mut sig).ok()?;
                if &sig == b"Exif\0\0" {
                    break pos + 4 + 6;
                }
            }
            // 0xDA = start of scan: no EXIF before the pixel data.
            if marker[1] == 0xDA || len < 2 {
                return None;
            }
            pos += 2 + len;
        }
    } else if matches!(&head[0..4], b"II*\0" | b"MM\0*") || &head[0..2] == b"II" {
        0 // TIFF / ARW / NEF / CR2 / DNG: the file *is* the container
    } else {
        return None;
    };

    f.seek(SeekFrom::Start(tiff_base)).ok()?;
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr).ok()?;
    let le = &hdr[0..2] == b"II";
    let ifd0 = rd_u32(&hdr[4..8], le) as u64;
    if ifd0 < 8 {
        return None;
    }

    f.seek(SeekFrom::Start(tiff_base + ifd0)).ok()?;
    let mut cnt = [0u8; 2];
    f.read_exact(&mut cnt).ok()?;
    let n = rd_u16(&cnt, le);
    for i in 0..n as u64 {
        let entry = tiff_base + ifd0 + 2 + i * 12;
        f.seek(SeekFrom::Start(entry)).ok()?;
        let mut e = [0u8; 12];
        f.read_exact(&mut e).ok()?;
        if rd_u16(&e[0..2], le) == 0x0112 {
            // SHORT value: the 4-byte value field holds it in its first
            // two bytes, in both byte orders.
            return Some((entry + 8, le));
        }
    }
    None
}

/// Set the EXIF Orientation tag in place. `Ok(false)` = no tag to patch.
///
/// The file's timestamps are restored afterwards, so a rotation leaves the
/// creation AND modification dates untouched — sorting by date and
/// incremental backups are unaffected.
pub fn set_orientation(path: &Path, orientation: u16) -> std::io::Result<bool> {
    let Some((offset, le)) = find_orientation(path) else {
        return Ok(false);
    };
    let bytes = if le {
        orientation.to_le_bytes()
    } else {
        orientation.to_be_bytes()
    };
    // Remember the timestamps before touching the file.
    let meta = std::fs::metadata(path)?;
    let times = std::fs::FileTimes::new()
        .set_accessed(meta.accessed().unwrap_or_else(|_| std::time::SystemTime::now()))
        .set_modified(meta.modified()?);

    let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(&bytes)?;
    f.flush()?;
    f.set_times(times)?; // put the dates back
    Ok(true)
}

/// Capture time (EXIF DateTimeOriginal) as UNIX milliseconds, 0 if absent.
pub fn capture_time(path: &Path) -> i64 {
    // TIFF-based files (every raw format we know, plus TIFF itself) are read
    // ENTIRELY into memory by the EXIF crate - 25 to 60 MB per raw, just to
    // pick out a 20-byte date. Sorting a folder of raws by capture time meant
    // reading every byte of every file. Walk the two IFDs by hand instead.
    if let Some(ms) = tiff_capture_time(path) {
        return ms;
    }
    let Ok(file) = std::fs::File::open(path) else { return 0 };
    let mut r = std::io::BufReader::new(file);
    let Ok(d) = exif::Reader::new().read_from_container(&mut r) else { return 0 };
    for tag in [exif::Tag::DateTimeOriginal, exif::Tag::DateTimeDigitized, exif::Tag::DateTime] {
        if let Some(f) = d.get_field(tag, exif::In::PRIMARY) {
            if let exif::Value::Ascii(ref v) = f.value {
                if let Some(bytes) = v.first() {
                    if let Some(ms) = parse_exif_datetime(bytes) {
                        return ms;
                    }
                }
            }
        }
    }
    0
}

/// Capture time of a TIFF-based file, reading only the few hundred bytes it
/// takes: IFD0, then the Exif sub-IFD it points to. None when the file is not
/// TIFF-based, so the caller falls back to the generic reader.
fn tiff_capture_time(path: &Path) -> Option<i64> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr).ok()?;
    let le = match &hdr[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let ifd0 = rd_u32(&hdr[4..8], le) as u64;

    // Read the entries of one IFD as (tag, type, count, value-or-offset).
    let mut entries = |off: u64| -> Option<Vec<(u16, u16, u32, u32)>> {
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut cnt = [0u8; 2];
        f.read_exact(&mut cnt).ok()?;
        let n = rd_u16(&cnt, le).min(512) as usize;
        let mut raw = vec![0u8; n * 12];
        f.read_exact(&mut raw).ok()?;
        Some(
            raw.chunks_exact(12)
                .map(|e| (rd_u16(&e[0..2], le), rd_u16(&e[2..4], le), rd_u32(&e[4..8], le), rd_u32(&e[8..12], le)))
                .collect(),
        )
    };
    let ifd0_entries = entries(ifd0)?;
    let exif_ifd = ifd0_entries.iter().find(|e| e.0 == 0x8769).map(|e| e.3 as u64);
    let modify_date = ifd0_entries.iter().find(|e| e.0 == 0x0132).copied();
    let exif_entries = exif_ifd.and_then(|o| entries(o)).unwrap_or_default();

    // DateTimeOriginal, then DateTimeDigitized, then the file's DateTime.
    let candidates = [
        exif_entries.iter().find(|e| e.0 == 0x9003).copied(),
        exif_entries.iter().find(|e| e.0 == 0x9004).copied(),
        modify_date,
    ];
    for (_, typ, count, at) in candidates.into_iter().flatten() {
        if typ != 2 || count < 19 || count > 64 {
            continue; // not the ASCII date we expect
        }
        let mut buf = vec![0u8; count as usize];
        if f.seek(SeekFrom::Start(at as u64)).is_err() || f.read_exact(&mut buf).is_err() {
            continue;
        }
        if let Some(ms) = parse_exif_datetime(&buf) {
            return Some(ms);
        }
    }
    // A TIFF without any date: say so, rather than loading the whole file.
    Some(0)
}

/// "YYYY:MM:DD HH:MM:SS" -> UNIX milliseconds (local time, as EXIF stores
/// no zone). Returns None on anything malformed.
fn parse_exif_datetime(b: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(b).ok()?;
    let s = s.trim_end_matches('\0').trim();
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.trim_start().parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec) * 1000)
}

/// Days since 1970-01-01 (Howard Hinnant's civil_from_days, inverted).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Current EXIF orientation (1 when absent or unreadable).
pub fn get_orientation(path: &Path) -> u16 {
    let Ok(file) = std::fs::File::open(path) else { return 1 };
    let mut r = std::io::BufReader::new(file);
    exif::Reader::new()
        .read_from_container(&mut r)
        .ok()
        .and_then(|d| {
            d.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1) as u16
}

/// EXIF orientation values in clockwise order, so rotating by a quarter
/// turn is just a step through this table.
const CW: [u16; 4] = [1, 6, 3, 8];

/// Orientation obtained by rotating `current` by `quarters` * 90 degrees
/// clockwise. Mirrored orientations (2/4/5/7) are normalised to upright.
pub fn rotate_orientation(current: u16, quarters: i32) -> u16 {
    let idx = CW.iter().position(|&o| o == current).unwrap_or(0) as i32;
    CW[(((idx + quarters) % 4 + 4) % 4) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Compare the hand walker with the EXIF crate on a real file (SD_PROBE).
    #[test]
    #[ignore]
    fn probe_capture_time() {
        let Ok(p) = std::env::var("SD_PROBE") else { return };
        let p = std::path::Path::new(&p);
        let t = std::time::Instant::now();
        let fast = tiff_capture_time(p);
        let t_fast = t.elapsed();
        let t = std::time::Instant::now();
        let file = std::fs::File::open(p).unwrap();
        let mut r = std::io::BufReader::new(file);
        let slow = exif::Reader::new().read_from_container(&mut r).ok().and_then(|d| {
            d.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY).and_then(|f| match f.value {
                exif::Value::Ascii(ref v) => v.first().and_then(|b| parse_exif_datetime(b)),
                _ => None,
            })
        });
        println!("walker {:?} in {:?} | crate {:?} in {:?}", fast, t_fast, slow, t.elapsed());
        assert_eq!(fast, slow, "the walker must agree with the crate");
    }

    /// The hand-written walker must find DateTimeOriginal in the Exif
    /// sub-IFD - the layout of every raw file - without the EXIF crate.
    #[test]
    fn tiff_capture_time_reads_the_exif_sub_ifd() {
        // Header, IFD0 with one entry (ExifIFD pointer), Exif IFD with one
        // entry (DateTimeOriginal, ASCII, 20 bytes, stored at an offset).
        let mut b = Vec::new();
        b.extend(b"II*\0");
        b.extend(8u32.to_le_bytes()); // IFD0 at 8
        b.extend(1u16.to_le_bytes()); // one entry
        b.extend(0x8769u16.to_le_bytes());
        b.extend(4u16.to_le_bytes()); // LONG
        b.extend(1u32.to_le_bytes());
        b.extend(26u32.to_le_bytes()); // Exif IFD at 26
        b.extend(0u32.to_le_bytes()); // no next IFD
        assert_eq!(b.len(), 26);
        b.extend(1u16.to_le_bytes());
        b.extend(0x9003u16.to_le_bytes());
        b.extend(2u16.to_le_bytes()); // ASCII
        b.extend(20u32.to_le_bytes());
        b.extend(44u32.to_le_bytes()); // value at 44
        b.extend(0u32.to_le_bytes());
        assert_eq!(b.len(), 44);
        b.extend(b"2014:05:12 18:34:07\0");
        b.extend(vec![0u8; 4096]); // a body the walker must never need

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("synthetic.arw");
        std::fs::write(&p, &b).unwrap();

        let ms = capture_time(&p);
        let expected = parse_exif_datetime(b"2014:05:12 18:34:07").unwrap();
        assert_eq!(ms, expected);
    }

    /// Minimal little-endian TIFF carrying an Orientation tag set to 1.
    fn fake_tiff() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"II*\0");
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // one entry
        out.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        out.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        out.extend_from_slice(&1u32.to_le_bytes()); // count
        out.extend_from_slice(&1u32.to_le_bytes()); // value = 1
        out.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        out
    }

    #[test]
    fn patches_orientation_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fake.arw");
        let before = fake_tiff();
        std::fs::File::create(&p).unwrap().write_all(&before).unwrap();

        assert!(set_orientation(&p, 6).unwrap());
        let after = std::fs::read(&p).unwrap();
        // Same length: nothing but the two value bytes changed.
        assert_eq!(before.len(), after.len());
        let diff: Vec<usize> = (0..before.len()).filter(|&i| before[i] != after[i]).collect();
        assert_eq!(diff, vec![18]);
        assert_eq!(after[18], 6);
    }

    #[test]
    fn file_dates_survive_the_patch() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("dated.arw");
        std::fs::File::create(&p).unwrap().write_all(&fake_tiff()).unwrap();
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));

        assert!(set_orientation(&p, 6).unwrap());
        let after = std::fs::metadata(&p).unwrap().modified().unwrap();
        let drift = after.duration_since(before).map(|d| d.as_millis()).unwrap_or(0);
        assert!(drift < 100, "modification date moved by {drift} ms");
    }

    #[test]
    fn parses_exif_timestamps() {
        // 2026-08-02 11:31:23 UTC = 1785670283
        assert_eq!(
            parse_exif_datetime(b"2026:08:02 11:31:23\0"),
            Some(1_785_670_283_000)
        );
        assert_eq!(parse_exif_datetime(b"garbage"), None);
        assert_eq!(parse_exif_datetime(b"1970:01:01 00:00:00"), Some(0));
    }

    #[test]
    fn quarter_turns_walk_the_table() {
        assert_eq!(rotate_orientation(1, 1), 6);
        assert_eq!(rotate_orientation(6, 1), 3);
        assert_eq!(rotate_orientation(1, -1), 8);
        assert_eq!(rotate_orientation(1, 4), 1);
    }

    #[test]
    fn missing_tag_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("plain.bin");
        std::fs::write(&p, b"not an image at all").unwrap();
        assert!(!set_orientation(&p, 6).unwrap_or(false));
    }
}

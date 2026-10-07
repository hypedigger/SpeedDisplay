// RAW preview extraction.
//
// Camera RAW files (Sony ARW, Nikon NEF, Canon CR2, Adobe DNG, Olympus ORF,
// Panasonic RW2...) are TIFF containers that embed one or more full JPEG
// previews. Extracting that JPEG takes milliseconds, versus hundreds of
// milliseconds for a full demosaic - this is exactly what ACDSee does.
//
// Strategy:
//   1. Parse the TIFF structure (IFD chain + SubIFDs) and collect every
//      JPEG span advertised by tags 0x0201/0x0202 (JPEGInterchangeFormat)
//      or 0x0111/0x0117 (StripOffsets) when Compression == 6/7 (JPEG).
//   2. Keep the largest span that starts with a JPEG SOI marker.
//   3. Fallback for non-TIFF RAW (CR3, RAF...): scan the first megabytes
//      for the largest well-formed JPEG (SOI..EOI).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];

/// Extract the largest embedded JPEG preview from a RAW file.
pub fn extract_preview(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;

    let is_tiff = matches!(&header[0..4], b"II*\0" | b"MM\0*")
        // Panasonic RW2 uses a non-standard magic (II U 0x00).
        || (&header[0..2] == b"II" && header[2] == 0x55);

    if is_tiff {
        if let Some(jpeg) = extract_from_tiff(&mut file) {
            return Some(jpeg);
        }
    }
    // Fallback: brute scan (handles CR3, RAF and exotic layouts).
    scan_for_jpeg(&mut file)
}

/// Candidate JPEG span inside the container.
struct Span {
    offset: u64,
    length: u64,
}

fn extract_from_tiff(file: &mut File) -> Option<Vec<u8>> {
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).ok()?;
    let little = &magic[0..2] == b"II";
    let first_ifd = read_u32(&magic[4..8], little) as u64;

    let mut spans: Vec<Span> = Vec::new();
    let mut queue: Vec<u64> = vec![first_ifd];
    let mut visited: Vec<u64> = Vec::new();

    // Walk the IFD chain and any SubIFDs, collecting preview spans.
    while let Some(ifd_off) = queue.pop() {
        if ifd_off == 0 || visited.contains(&ifd_off) || visited.len() > 64 {
            continue;
        }
        visited.push(ifd_off);
        if file.seek(SeekFrom::Start(ifd_off)).is_err() {
            continue;
        }
        let mut cnt = [0u8; 2];
        if file.read_exact(&mut cnt).is_err() {
            continue;
        }
        let n = read_u16(&cnt, little) as u64;
        if n == 0 || n > 512 {
            continue;
        }

        let mut jpeg_off: Option<u64> = None;
        let mut jpeg_len: Option<u64> = None;
        let mut strip_off: Option<u64> = None;
        let mut strip_len: Option<u64> = None;
        let mut compression: u32 = 0;

        for i in 0..n {
            if file.seek(SeekFrom::Start(ifd_off + 2 + i * 12)).is_err() {
                break;
            }
            let mut e = [0u8; 12];
            if file.read_exact(&mut e).is_err() {
                break;
            }
            let tag = read_u16(&e[0..2], little);
            let typ = read_u16(&e[2..4], little);
            let count = read_u32(&e[4..8], little);
            let value = read_u32(&e[8..12], little) as u64;

            match tag {
                0x0103 => compression = value as u32,        // Compression
                0x0201 => jpeg_off = Some(value),            // JPEGInterchangeFormat
                0x0202 => jpeg_len = Some(value),            // ...Length
                0x0111 => strip_off = Some(value),           // StripOffsets
                0x0117 => strip_len = Some(value),           // StripByteCounts
                0x014A | 0x0190 => {
                    // SubIFDs: one or several u32 offsets.
                    if typ == 4 || typ == 13 {
                        if count == 1 {
                            queue.push(value);
                        } else {
                            let mut buf = vec![0u8; (count as usize) * 4];
                            let save = file.stream_position().ok();
                            if file.seek(SeekFrom::Start(value)).is_ok()
                                && file.read_exact(&mut buf).is_ok()
                            {
                                for c in buf.chunks_exact(4) {
                                    queue.push(read_u32(c, little) as u64);
                                }
                            }
                            if let Some(p) = save {
                                let _ = file.seek(SeekFrom::Start(p));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let (Some(o), Some(l)) = (jpeg_off, jpeg_len) {
            spans.push(Span { offset: o, length: l });
        }
        // Single JPEG-compressed strip (common in CR2 IFD0).
        if (compression == 6 || compression == 7) && strip_off.is_some() {
            if let (Some(o), Some(l)) = (strip_off, strip_len) {
                spans.push(Span { offset: o, length: l });
            }
        }

        // Next IFD in the chain.
        if file
            .seek(SeekFrom::Start(ifd_off + 2 + n * 12))
            .is_ok()
        {
            let mut nx = [0u8; 4];
            if file.read_exact(&mut nx).is_ok() {
                queue.push(read_u32(&nx, little) as u64);
            }
        }
    }

    // Pick the largest span that really is a JPEG.
    spans.sort_by(|a, b| b.length.cmp(&a.length));
    for s in spans {
        if s.length < 4 || s.length > 128 * 1024 * 1024 {
            continue;
        }
        if file.seek(SeekFrom::Start(s.offset)).is_err() {
            continue;
        }
        let mut data = vec![0u8; s.length as usize];
        if file.read_exact(&mut data).is_err() {
            continue;
        }
        if data[0..2] == SOI {
            return Some(data);
        }
    }
    None
}

/// Brute-force scan of the first 32 MiB for the largest SOI..EOI JPEG.
fn scan_for_jpeg(file: &mut File) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut buf = Vec::with_capacity(8 * 1024 * 1024);
    file.take(32 * 1024 * 1024).read_to_end(&mut buf).ok()?;

    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i + 4 < buf.len() {
        if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
            // Find the matching EOI.
            let mut j = i + 2;
            while j + 1 < buf.len() {
                if buf[j] == EOI[0] && buf[j + 1] == EOI[1] {
                    let len = j + 2 - i;
                    if len > 8 * 1024 && best.map(|(_, l)| len > l).unwrap_or(true) {
                        best = Some((i, len));
                    }
                    break;
                }
                j += 1;
            }
            i = if let Some((s, l)) = best { s + l } else { i + 2 };
        } else {
            i += 1;
        }
    }
    best.map(|(s, l)| buf[s..s + l].to_vec())
}

fn read_u16(b: &[u8], little: bool) -> u16 {
    if little {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    }
}

fn read_u32(b: &[u8], little: bool) -> u32 {
    if little {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal little-endian TIFF embedding one JPEG via 0x0201/0x0202,
    /// mimicking how Sony ARW exposes its preview.
    fn build_fake_arw(jpeg: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"II*\0");
        out.extend_from_slice(&8u32.to_le_bytes()); // first IFD at offset 8

        let n_entries: u16 = 2;
        let ifd_size = 2 + 12 * n_entries as u32 + 4;
        let jpeg_offset = 8 + ifd_size;

        out.extend_from_slice(&n_entries.to_le_bytes());
        // Tag 0x0201 JPEGInterchangeFormat (LONG, 1, offset)
        out.extend_from_slice(&0x0201u16.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&jpeg_offset.to_le_bytes());
        // Tag 0x0202 JPEGInterchangeFormatLength
        out.extend_from_slice(&0x0202u16.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
        // Next IFD = 0
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(jpeg);
        out
    }

    fn tiny_jpeg() -> Vec<u8> {
        // Not a decodable image, but a structurally valid SOI..EOI stream
        // large enough to pass sanity checks.
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0];
        j.extend_from_slice(&[0x00; 16]);
        j.extend(std::iter::repeat(0xAB).take(9000));
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn extracts_jpeg_from_tiff_container() {
        let jpeg = tiny_jpeg();
        let arw = build_fake_arw(&jpeg);
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fake.arw");
        std::fs::File::create(&p).unwrap().write_all(&arw).unwrap();

        let got = extract_preview(&p).expect("preview expected");
        assert_eq!(got, jpeg);
    }

    #[test]
    fn fallback_scan_finds_jpeg() {
        // Non-TIFF container: JPEG buried after a random header (like CR3/RAF).
        let jpeg = tiny_jpeg();
        let mut blob = vec![0x00u8; 4096];
        blob[0..4].copy_from_slice(b"XXXX");
        blob.extend_from_slice(&jpeg);
        blob.extend_from_slice(&[0u8; 512]);

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("fake.cr3");
        std::fs::File::create(&p).unwrap().write_all(&blob).unwrap();

        let got = extract_preview(&p).expect("preview expected");
        assert_eq!(got, jpeg);
    }
}

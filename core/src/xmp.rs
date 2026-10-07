// XMP sidecar files.
//
// Ratings, colour labels and keywords are written next to the original as
// `<file>.xmp`, the format Lightroom, Bridge, ACDSee and XnView all read.
// The media file itself is never opened for writing, so its bytes AND its
// timestamps are untouched — the same guarantee as the rotation feature,
// obtained here for free.

use std::path::{Path, PathBuf};

/// Metadata that travels with a picture.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct XmpMeta {
    /// 0-5, -1 meaning "rejected" in Adobe's convention.
    pub rating: i64,
    /// Colour label as a word ("Red", "Yellow"...); empty when unset.
    pub label: String,
    /// Keywords, comma-separated.
    pub tags: String,
    /// EXIF orientation (1-8), 0 when not recorded. For a RAW file this is
    /// THE place Lightroom, Bridge and Capture One look: they keep their own
    /// idea of a raw file's orientation and ignore the container's tag.
    pub orientation: u16,
}

/// Path of the sidecar for a media file: `DSC001.ARW` -> `DSC001.ARW.xmp`.
pub fn sidecar_path(media: &Path) -> PathBuf {
    let mut name = media.file_name().unwrap_or_default().to_os_string();
    name.push(".xmp");
    media.with_file_name(name)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Serialise to the XMP packet layout Adobe tools expect.
pub fn to_xml(m: &XmpMeta) -> String {
    let mut props = String::new();
    if m.rating != 0 {
        props.push_str(&format!("\n    xmp:Rating=\"{}\"", m.rating));
    }
    if !m.label.is_empty() {
        props.push_str(&format!("\n    xmp:Label=\"{}\"", escape(&m.label)));
    }
    if m.orientation != 0 {
        props.push_str(&format!("\n    tiff:Orientation=\"{}\"", m.orientation));
    }
    let keywords: Vec<&str> = m
        .tags
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();
    let subject = if keywords.is_empty() {
        String::new()
    } else {
        let items: String = keywords
            .iter()
            .map(|k| format!("\n     <rdf:li>{}</rdf:li>", escape(k)))
            .collect();
        format!("\n    <dc:subject>\n     <rdf:Bag>{items}\n     </rdf:Bag>\n    </dc:subject>")
    };

    format!(
        r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="SpeedDisplay">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"{props}>{subject}
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#
    )
}

fn attr<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let start = xml.find(&key)? + key.len();
    let rest = &xml[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Parse the subset of XMP we care about. Unknown markup is ignored.
pub fn from_xml(xml: &str) -> XmpMeta {
    let mut m = XmpMeta::default();
    if let Some(v) = attr(xml, "xmp:Rating") {
        m.rating = v.trim().parse().unwrap_or(0);
    }
    if let Some(v) = attr(xml, "xmp:Label") {
        m.label = unescape(v);
    }
    if let Some(v) = attr(xml, "tiff:Orientation") {
        m.orientation = v.trim().parse().unwrap_or(0);
    }
    // Keywords live in <dc:subject><rdf:Bag><rdf:li>...
    if let Some(start) = xml.find("<dc:subject>") {
        let block = &xml[start..];
        let block = &block[..block.find("</dc:subject>").unwrap_or(block.len())];
        let mut keys = Vec::new();
        let mut rest = block;
        while let Some(i) = rest.find("<rdf:li>") {
            rest = &rest[i + 8..];
            if let Some(j) = rest.find("</rdf:li>") {
                keys.push(unescape(rest[..j].trim()));
                rest = &rest[j + 9..];
            } else {
                break;
            }
        }
        m.tags = keys.join(", ");
    }
    m
}

/// Write (or delete, when everything is empty) the sidecar of `media`.
pub fn write(media: &Path, m: &XmpMeta) -> std::io::Result<()> {
    let side = sidecar_path(media);
    if m.rating == 0 && m.label.is_empty() && m.tags.trim().is_empty() && m.orientation == 0 {
        // Nothing left to record: don't leave an empty file behind.
        if side.exists() {
            std::fs::remove_file(&side)?;
        }
        return Ok(());
    }
    std::fs::write(&side, to_xml(m))
}

/// Read the sidecar of `media`, if any.
pub fn read(media: &Path) -> Option<XmpMeta> {
    let xml = std::fs::read_to_string(sidecar_path(media)).ok()?;
    Some(from_xml(&xml))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let m = XmpMeta {
            rating: 4,
            label: "Red".into(),
            tags: "vacances, mer & sable, <test>".into(),
            orientation: 6,
        };
        let back = from_xml(&to_xml(&m));
        assert_eq!(back.rating, 4);
        assert_eq!(back.label, "Red");
        assert_eq!(back.tags, "vacances, mer & sable, <test>");
        assert_eq!(back.orientation, 6);
    }

    /// A rotation alone is enough to justify a sidecar: that is the only
    /// channel a raw file has towards Lightroom & co.
    #[test]
    fn orientation_only_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let media = tmp.path().join("DSC001.ARW");
        std::fs::write(&media, b"raw bytes").unwrap();
        write(&media, &XmpMeta { orientation: 8, ..Default::default() }).unwrap();
        let xml = std::fs::read_to_string(sidecar_path(&media)).unwrap();
        assert!(xml.contains("tiff:Orientation=\"8\""), "{xml}");
        assert!(xml.contains("xmlns:tiff="));
        assert_eq!(read(&media).unwrap().orientation, 8);
        // The raw file itself must not have been touched.
        assert_eq!(std::fs::read(&media).unwrap(), b"raw bytes");
    }

    #[test]
    fn sidecar_is_written_next_to_the_untouched_original() {
        let tmp = tempfile::tempdir().unwrap();
        let media = tmp.path().join("DSC001.ARW");
        std::fs::write(&media, b"raw bytes").unwrap();
        let before = std::fs::metadata(&media).unwrap();

        write(&media, &XmpMeta { rating: 5, ..Default::default() }).unwrap();
        let side = tmp.path().join("DSC001.ARW.xmp");
        assert!(side.exists());
        assert_eq!(read(&media).unwrap().rating, 5);

        // The picture itself was never opened for writing.
        let after = std::fs::metadata(&media).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    }

    #[test]
    fn clearing_everything_removes_the_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let media = tmp.path().join("a.jpg");
        std::fs::write(&media, b"jpeg").unwrap();
        write(&media, &XmpMeta { rating: 3, ..Default::default() }).unwrap();
        assert!(sidecar_path(&media).exists());

        write(&media, &XmpMeta::default()).unwrap();
        assert!(!sidecar_path(&media).exists());
        assert!(read(&media).is_none());
    }

    #[test]
    fn empty_meta_produces_no_properties() {
        let xml = to_xml(&XmpMeta::default());
        assert!(!xml.contains("xmp:Rating"));
        assert!(!xml.contains("dc:subject"));
    }
}

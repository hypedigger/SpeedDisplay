// Lightroom-compatible metadata: rating, hierarchical keywords, orientation
// and straightening, written where Lightroom Classic actually looks at import.
//
// * JPEG: the XMP packet is embedded in the file (APP1 segment). Lightroom
//   ignores sidecars for JPEG. The file's creation, modification and access
//   times are restored afterwards; only its bytes change.
// * Everything else (RAW, video...): Adobe-style sidecar `DSC001.xmp`, NOT
//   `DSC001.ARW.xmp`, which Lightroom does not recognise. The media file is
//   never opened for writing.
//
// An existing packet (camera XMP, Ultra HDR gain-map description, a sidecar
// written by Lightroom with all its develop settings...) is preserved: only
// the properties we manage are removed and re-added in their own
// rdf:Description block.
//
// The capture date (EXIF DateTimeOriginal) is never touched.

use std::path::{Path, PathBuf};

const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Process version written when a packet has no develop settings yet
/// (current Lightroom process, "Version 6"). Without it Lightroom could
/// fall back to a legacy process and render the photo differently.
const PROCESS_VERSION: &str = "15.4";

pub const JPEG_EXTS: &[&str] = &["jpg", "jpeg", "jpe", "jfif"];

/// Straightening as Lightroom stores it: an angle plus the crop rectangle.
/// The crop corners are normalised (0-1) in the unrotated image frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Straighten {
    pub angle: f64,
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// What a picture currently carries.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// 0-5.
    pub rating: i64,
    /// Keywords, as Lightroom paths: `People|Alice`, `street art`.
    pub keywords: Vec<String>,
    /// EXIF orientation 1-8, 0 when not recorded in the XMP.
    pub orientation: u16,
    pub straighten: Option<Straighten>,
}

/// A change to apply. `None` = leave that property alone.
#[derive(Debug, Clone, Default)]
pub struct Edit {
    /// `Some(0)` removes the rating.
    pub rating: Option<i64>,
    /// Full replacement list (paths with `|`). `Some(vec![])` removes all.
    pub keywords: Option<Vec<String>>,
    pub orientation: Option<u16>,
    /// `Some(None)` removes the straightening (and any crop).
    pub straighten: Option<Option<Straighten>>,
}

pub fn is_jpeg(media: &Path) -> bool {
    media
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| JPEG_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Adobe sidecar name: `DSC001.ARW` -> `DSC001.xmp`.
pub fn sidecar_path(media: &Path) -> PathBuf {
    media.with_extension("xmp")
}

/// Straightening of `angle_deg` (Lightroom's CropAngle) with the largest
/// crop of the same aspect ratio that stays inside the rotated picture.
/// `aspect` = width / height of the unrotated image.
///
/// Verified against Lightroom Classic (phase 0): the crop corners are the
/// top-left and bottom-right corners of the crop box *as it sits on the
/// unrotated image*, i.e. rotated by +CropAngle in image coordinates (y
/// down). A positive CropAngle therefore turns the picture counter-clockwise.
/// With this convention Lightroom reproduces the expected crop size exactly.
pub fn straighten_crop(aspect: f64, angle_deg: f64) -> Straighten {
    let (w, h) = (aspect, 1.0);
    let t = angle_deg.abs().to_radians();
    let (c, s) = (t.cos(), t.sin());
    let k = (w / (w * c + h * s)).min(h / (w * s + h * c));
    let (cw, ch) = (k * w, k * h);
    let phi = angle_deg.to_radians();
    let rot = |x: f64, y: f64| (x * phi.cos() - y * phi.sin(), x * phi.sin() + y * phi.cos());
    let (x0, y0) = rot(-cw / 2.0, -ch / 2.0);
    let (x1, y1) = rot(cw / 2.0, ch / 2.0);
    Straighten {
        angle: angle_deg,
        left: 0.5 + x0 / w,
        top: 0.5 + y0 / h,
        right: 0.5 + x1 / w,
        bottom: 0.5 + y1 / h,
    }
}

// ---------------------------------------------------------------- XMP text

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn is_name_end(c: Option<char>) -> bool {
    matches!(c, None | Some('>' | '/' | ' ' | '\t' | '\r' | '\n'))
}

/// Attribute form: ` qname="v"` (preceded by whitespace). Returns the span
/// including the leading whitespace and the value.
fn find_attr(xml: &str, qname: &str, from: usize) -> Option<(usize, usize, String)> {
    let mut pos = from;
    while let Some(i) = xml[pos..].find(qname) {
        let start = pos + i;
        let after = start + qname.len();
        let before_ws = xml[..start].chars().last().map(|c| c.is_whitespace()).unwrap_or(false);
        let rest = &xml[after..];
        let quote = rest.trim_start().strip_prefix('=').map(str::trim_start).and_then(|r| r.chars().next());
        if before_ws {
            if let Some(q @ ('"' | '\'')) = quote {
                let vstart = after + rest.find(q).unwrap() + 1;
                if let Some(vlen) = xml[vstart..].find(q) {
                    let ws_start = xml[..start].trim_end().len();
                    return Some((ws_start, vstart + vlen + 1, unescape(&xml[vstart..vstart + vlen])));
                }
            }
        }
        pos = after;
    }
    None
}

/// Element form: `<qname ...>inner</qname>` or `<qname .../>`. Returns the
/// span (leading indentation included) and the inner text.
fn find_elem(xml: &str, qname: &str, from: usize) -> Option<(usize, usize, String)> {
    let open = format!("<{qname}");
    let close = format!("</{qname}>");
    let mut pos = from;
    while let Some(i) = xml[pos..].find(&open) {
        let start = pos + i;
        let after = start + open.len();
        if !is_name_end(xml[after..].chars().next()) {
            pos = after;
            continue;
        }
        let gt = after + xml[after..].find('>')?;
        let ws_start = xml[..start].trim_end_matches([' ', '\t']).len();
        if xml[..gt].ends_with('/') {
            return Some((ws_start, gt + 1, String::new()));
        }
        let end = gt + 1 + xml[gt + 1..].find(&close)?;
        return Some((ws_start, end + close.len(), xml[gt + 1..end].to_string()));
    }
    None
}

/// Value of a simple property, whichever form it is written in.
fn get_prop(xml: &str, qname: &str) -> Option<String> {
    if let Some((_, _, v)) = find_attr(xml, qname, 0) {
        return Some(v);
    }
    find_elem(xml, qname, 0).map(|(_, _, v)| unescape(v.trim()))
}

/// Items of an rdf:Bag / rdf:Seq property.
fn get_list(xml: &str, qname: &str) -> Vec<String> {
    let Some((_, _, inner)) = find_elem(xml, qname, 0) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((_, end, v)) = find_elem(&inner, "rdf:li", pos) {
        let v = unescape(v.trim());
        if !v.is_empty() {
            out.push(v);
        }
        pos = end;
    }
    out
}

/// Remove every occurrence of a property, attribute or element form.
fn remove_prop(xml: &mut String, qname: &str) {
    while let Some((a, b, _)) = find_attr(xml, qname, 0) {
        xml.replace_range(a..b, "");
    }
    while let Some((a, b, _)) = find_elem(xml, qname, 0) {
        // Drop the now-empty line as well.
        let b = if xml[b..].starts_with("\r\n") { b + 2 } else if xml[b..].starts_with('\n') { b + 1 } else { b };
        xml.replace_range(a..b, "");
    }
}

/// Drop rdf:Description blocks left with no property at all (typically our
/// own block from a previous write, once its properties were removed).
fn drop_empty_descriptions(xml: &mut String) {
    let mut pos = 0;
    while let Some((a, b, inner)) = find_elem(xml, "rdf:Description", pos) {
        let open_end = a + xml[a..].find('>').unwrap_or(0);
        let tag = &xml[a..open_end];
        let mut attrs_only_decls = true;
        let mut p = tag.find("rdf:Description").map(|i| i + "rdf:Description".len()).unwrap_or(tag.len());
        while let Some(eq) = tag[p..].find('=') {
            let name = tag[p..p + eq].trim();
            if !(name == "rdf:about" || name.starts_with("xmlns:")) {
                attrs_only_decls = false;
                break;
            }
            let rest = &tag[p + eq + 1..];
            let q = rest.trim_start().chars().next().unwrap_or('"');
            let vs = p + eq + 1 + rest.find(q).unwrap_or(0) + 1;
            p = vs + tag[vs..].find(q).unwrap_or(tag.len() - vs) + 1;
        }
        if attrs_only_decls && inner.trim().is_empty() {
            let b = if xml[b..].starts_with("\r\n") { b + 2 } else if xml[b..].starts_with('\n') { b + 1 } else { b };
            xml.replace_range(a..b, "");
            pos = a;
        } else {
            pos = b;
        }
    }
}

/// Parse what we manage out of an XMP packet.
pub fn parse(xml: &str) -> Meta {
    let mut m = Meta::default();
    m.rating = get_prop(xml, "xmp:Rating").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    m.orientation = get_prop(xml, "tiff:Orientation").and_then(|v| v.trim().parse().ok()).unwrap_or(0);

    let paths = get_list(xml, "lr:hierarchicalSubject");
    let flat = get_list(xml, "dc:subject");
    let mut kw: Vec<String> = Vec::new();
    for p in &paths {
        if !kw.contains(p) {
            kw.push(p.clone());
        }
    }
    for f in flat {
        let covered = paths.iter().any(|p| p.split('|').any(|seg| seg == f));
        if !covered && !kw.contains(&f) {
            kw.push(f);
        }
    }
    m.keywords = kw;

    let has_crop = get_prop(xml, "crs:HasCrop").map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false);
    if has_crop {
        let num = |q: &str| get_prop(xml, q).and_then(|v| v.trim().parse::<f64>().ok());
        m.straighten = Some(Straighten {
            angle: num("crs:CropAngle").unwrap_or(0.0),
            left: num("crs:CropLeft").unwrap_or(0.0),
            top: num("crs:CropTop").unwrap_or(0.0),
            right: num("crs:CropRight").unwrap_or(1.0),
            bottom: num("crs:CropBottom").unwrap_or(1.0),
        });
    }
    m
}

const EMPTY_PACKET: &str = concat!(
    "<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"SpeedDisplay\">\n",
    " <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
    " </rdf:RDF>\n",
    "</x:xmpmeta>\n",
    "<?xpacket end=\"w\"?>"
);

fn fmt_num(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn bag(qname: &str, items: &[String]) -> String {
    let lis: String = items.iter().map(|k| format!("\n     <rdf:li>{}</rdf:li>", escape(k))).collect();
    format!("\n   <{qname}>\n    <rdf:Bag>{lis}\n    </rdf:Bag>\n   </{qname}>")
}

/// Apply `edit` to an existing packet (or a fresh one) and return the result.
pub fn merge(existing: Option<&str>, edit: &Edit) -> String {
    let mut xml = existing
        .filter(|x| x.contains("</rdf:RDF>"))
        .unwrap_or(EMPTY_PACKET)
        .to_string();
    let mut ns: Vec<(&str, &str)> = Vec::new();
    let mut attrs = String::new();
    let mut elems = String::new();

    if let Some(r) = edit.rating {
        remove_prop(&mut xml, "xmp:Rating");
        if r != 0 {
            ns.push(("xmp", "http://ns.adobe.com/xap/1.0/"));
            attrs.push_str(&format!("\n    xmp:Rating=\"{}\"", r.clamp(-1, 5)));
        }
    }
    if let Some(o) = edit.orientation {
        remove_prop(&mut xml, "tiff:Orientation");
        if o != 0 {
            ns.push(("tiff", "http://ns.adobe.com/tiff/1.0/"));
            attrs.push_str(&format!("\n    tiff:Orientation=\"{o}\""));
        }
    }
    if let Some(kws) = &edit.keywords {
        remove_prop(&mut xml, "dc:subject");
        remove_prop(&mut xml, "lr:hierarchicalSubject");
        let mut paths: Vec<String> = Vec::new();
        let mut flat: Vec<String> = Vec::new();
        for k in kws {
            let segs: Vec<String> = k.split('|').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if segs.is_empty() {
                continue;
            }
            let path = segs.join("|");
            if !paths.contains(&path) {
                paths.push(path);
            }
            // Like Lightroom's default export: parents are listed too.
            for s in segs {
                if !flat.contains(&s) {
                    flat.push(s);
                }
            }
        }
        if !paths.is_empty() {
            ns.push(("dc", "http://purl.org/dc/elements/1.1/"));
            ns.push(("lr", "http://ns.adobe.com/lightroom/1.0/"));
            elems.push_str(&bag("dc:subject", &flat));
            elems.push_str(&bag("lr:hierarchicalSubject", &paths));
        }
    }
    if let Some(st) = &edit.straighten {
        for q in ["crs:HasCrop", "crs:CropAngle", "crs:CropTop", "crs:CropLeft", "crs:CropBottom", "crs:CropRight"] {
            remove_prop(&mut xml, q);
        }
        if let Some(s) = st {
            ns.push(("crs", "http://ns.adobe.com/camera-raw-settings/1.0/"));
            if get_prop(&xml, "crs:ProcessVersion").is_none() {
                attrs.push_str(&format!("\n    crs:ProcessVersion=\"{PROCESS_VERSION}\""));
            }
            // Without HasSettings, Lightroom ignores the develop settings of
            // a JPEG/TIFF; AlreadyApplied="True" would mean "baked in".
            for q in ["crs:HasSettings", "crs:AlreadyApplied"] {
                remove_prop(&mut xml, q);
            }
            attrs.push_str("\n    crs:HasSettings=\"True\"\n    crs:AlreadyApplied=\"False\"");
            attrs.push_str(&format!(
                "\n    crs:HasCrop=\"True\"\n    crs:CropAngle=\"{}\"\n    crs:CropTop=\"{}\"\n    crs:CropLeft=\"{}\"\n    crs:CropBottom=\"{}\"\n    crs:CropRight=\"{}\"",
                fmt_num(s.angle), fmt_num(s.top), fmt_num(s.left), fmt_num(s.bottom), fmt_num(s.right)
            ));
        }
    }

    drop_empty_descriptions(&mut xml);
    if attrs.is_empty() && elems.is_empty() {
        return xml;
    }
    let decls: String = ns.iter().map(|(p, u)| format!("\n    xmlns:{p}=\"{u}\"")).collect();
    let block = format!(
        "  <rdf:Description rdf:about=\"\"{decls}{attrs}>{elems}\n  </rdf:Description>\n"
    );
    // Insert at the start of the line holding </rdf:RDF>, keeping its indent.
    let close = xml.rfind("</rdf:RDF>").unwrap();
    let at = match xml[..close].rfind('\n') {
        Some(nl) if xml[nl + 1..close].trim().is_empty() => nl + 1,
        _ => close,
    };
    xml.insert_str(at, &block);
    xml
}

// ------------------------------------------------------------- JPEG segments

/// Byte span of the embedded XMP APP1 segment, or where to insert one.
/// Returns (start, end, packet).
fn jpeg_xmp_span(b: &[u8]) -> std::io::Result<(usize, usize, Option<String>)> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a JPEG");
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return Err(bad());
    }
    let mut pos = 2;
    let mut insert_at = 2;
    let mut leading = true;
    while pos + 4 <= b.len() {
        if b[pos] != 0xFF {
            return Err(bad());
        }
        let marker = b[pos + 1];
        if marker == 0xFF {
            pos += 1; // fill byte
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break; // pixel data: no metadata after this
        }
        let len = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
        let end = pos + 2 + len;
        if len < 2 || end > b.len() {
            return Err(bad());
        }
        let data = &b[pos + 4..end];
        if marker == 0xE1 && data.starts_with(XMP_SIG) {
            let packet = String::from_utf8_lossy(&data[XMP_SIG.len()..]).into_owned();
            return Ok((pos, end, Some(packet)));
        }
        // Insert after the leading APP0/APP1 run (JFIF, Exif), so before any
        // APP2 MPF directory: offsets to an Ultra HDR gain map stay valid.
        if leading && (marker == 0xE0 || marker == 0xE1) {
            insert_at = end;
        } else {
            leading = false;
        }
        pos = end;
    }
    Ok((insert_at, insert_at, None))
}

fn read_jpeg_packet(media: &Path) -> Option<String> {
    let b = std::fs::read(media).ok()?;
    jpeg_xmp_span(&b).ok()?.2
}

/// File timestamps, to put back after a rewrite.
struct Times {
    modified: std::time::SystemTime,
    accessed: std::time::SystemTime,
    #[cfg(windows)]
    created: Option<std::time::SystemTime>,
}

fn capture_times(p: &Path) -> std::io::Result<Times> {
    let m = std::fs::metadata(p)?;
    Ok(Times {
        modified: m.modified()?,
        accessed: m.accessed().unwrap_or_else(|_| std::time::SystemTime::now()),
        #[cfg(windows)]
        created: m.created().ok(),
    })
}

fn restore_times(p: &Path, t: &Times) -> std::io::Result<()> {
    let mut ft = std::fs::FileTimes::new().set_modified(t.modified).set_accessed(t.accessed);
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTimesExt;
        if let Some(c) = t.created {
            ft = ft.set_created(c);
        }
    }
    std::fs::OpenOptions::new().write(true).open(p)?.set_times(ft)
}

fn write_jpeg(media: &Path, edit: &Edit) -> std::io::Result<()> {
    let times = capture_times(media)?;
    if let Some(o) = edit.orientation {
        // The EXIF tag is what every viewer reads; the XMP copy keeps them in step.
        crate::exifwrite::set_orientation(media, o.max(1))?;
    }
    let bytes = std::fs::read(media)?;
    let (start, end, packet) = jpeg_xmp_span(&bytes)?;
    let xml = merge(packet.as_deref(), edit);
    let seg_len = 2 + XMP_SIG.len() + xml.len();
    if seg_len > 0xFFFF {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "XMP packet too large"));
    }
    let mut out = Vec::with_capacity(bytes.len() + xml.len() + 64);
    out.extend_from_slice(&bytes[..start]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&(seg_len as u16).to_be_bytes());
    out.extend_from_slice(XMP_SIG);
    out.extend_from_slice(xml.as_bytes());
    out.extend_from_slice(&bytes[end..]);

    // Write next to the original, then swap: a crash never leaves a half file.
    let mut tmp = media.as_os_str().to_os_string();
    tmp.push(".sdtmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &out)?;
    if let Err(e) = std::fs::rename(&tmp, media) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    restore_times(media, &times)
}

fn write_sidecar(media: &Path, edit: &Edit) -> std::io::Result<()> {
    let side = sidecar_path(media);
    let existing = std::fs::read_to_string(&side).ok();
    let existing = existing.as_deref().map(|s| s.trim_start_matches('\u{FEFF}'));
    let xml = merge(existing, edit);
    std::fs::write(&side, xml)
}

/// Read the managed metadata of a picture (embedded for JPEG, sidecar otherwise).
pub fn read(media: &Path) -> Meta {
    let xml = if is_jpeg(media) {
        read_jpeg_packet(media)
    } else {
        std::fs::read_to_string(sidecar_path(media)).ok()
    };
    xml.map(|x| parse(&x)).unwrap_or_default()
}

/// Apply `edit` where Lightroom will read it.
pub fn write(media: &Path, edit: &Edit) -> std::io::Result<()> {
    if is_jpeg(media) {
        write_jpeg(media, edit)
    } else {
        write_sidecar(media, edit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([200, 100, 50]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageOutputFormat::Jpeg(90)).unwrap();
        out.into_inner()
    }

    fn edit_all() -> Edit {
        Edit {
            rating: Some(4),
            keywords: Some(vec!["People|Alice".into(), "street art".into(), "Lieux|France|Château & co".into()]),
            orientation: Some(6),
            straighten: Some(Some(straighten_crop(1.5, 3.0))),
        }
    }

    #[test]
    fn sidecar_uses_adobe_naming() {
        assert_eq!(sidecar_path(Path::new("C:/x/DSC001.ARW")), PathBuf::from("C:/x/DSC001.xmp"));
    }

    #[test]
    fn merge_round_trip() {
        let xml = merge(None, &edit_all());
        let m = parse(&xml);
        assert_eq!(m.rating, 4);
        assert_eq!(m.orientation, 6);
        assert_eq!(m.keywords, vec!["People|Alice", "street art", "Lieux|France|Château & co"]);
        let s = m.straighten.unwrap();
        assert!((s.angle - 3.0).abs() < 1e-9);
        assert!(xml.contains("crs:ProcessVersion=\"15.4\""));
        assert!(xml.contains("<rdf:li>People</rdf:li>"), "parents listed in dc:subject");
    }

    #[test]
    fn merge_replaces_without_duplicates_and_keeps_foreign_data() {
        let lr = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
   xmp:Rating="0"
   crs:ProcessVersion="6.7"
   crs:Exposure2012="+0.35"
   crs:HasCrop="False"
   crs:AlreadyApplied="False">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>vieux</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        let xml = merge(Some(lr), &edit_all());
        assert_eq!(xml.matches("xmp:Rating=").count(), 1);
        assert_eq!(xml.matches("crs:HasCrop=").count(), 1);
        assert_eq!(xml.matches("<dc:subject>").count(), 1);
        assert!(xml.contains("crs:Exposure2012=\"+0.35\""), "develop settings kept");
        assert_eq!(xml.matches("crs:AlreadyApplied=").count(), 1);
        assert!(xml.contains("crs:Exposure2012=\"+0.35\">"), "tag still closed");
        assert!(xml.contains("crs:ProcessVersion=\"6.7\""), "existing process kept");
        assert!(!xml.contains("15.4"));
        assert!(!xml.contains("vieux"));
        // Applying the same edit twice is stable.
        assert_eq!(merge(Some(&xml), &edit_all()).matches("<rdf:Description").count(), 2);
    }

    #[test]
    fn clearing_removes_properties() {
        let xml = merge(None, &edit_all());
        let cleared = merge(
            Some(&xml),
            &Edit { rating: Some(0), keywords: Some(vec![]), orientation: Some(0), straighten: Some(None) },
        );
        assert_eq!(parse(&cleared), Meta::default());
    }

    #[test]
    fn straighten_crop_stays_inside() {
        for angle in [-5.0, 5.0, 12.0] {
            let s = straighten_crop(4.0 / 3.0, angle);
            for v in [s.left, s.top, s.right, s.bottom] {
                assert!((-1e-12..=1.0 + 1e-12).contains(&v), "{s:?}");
            }
        }
        // Values written for a 4928x3276 (EXIF) raw at 5 degrees: Lightroom
        // derived a 4357x2895 crop of its 4912x3264 frame, same aspect ratio.
        let s = straighten_crop(4928.0 / 3276.0, 5.0);
        assert!((s.left - 0.083848).abs() < 1e-6 && (s.right - 0.916152).abs() < 1e-6, "{s:?}");
        assert!(s.top.abs() < 1e-9 && (s.bottom - 1.0).abs() < 1e-9);
        let z = straighten_crop(1.5, 0.0);
        assert!(z.left.abs() < 1e-12 && (z.right - 1.0).abs() < 1e-12);
    }

    #[test]
    fn jpeg_embed_keeps_pixels_and_dates() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.jpg");
        let orig = tiny_jpeg();
        std::fs::write(&p, &orig).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();

        write(&p, &Edit { rating: Some(5), keywords: Some(vec!["a|b".into()]), ..Default::default() }).unwrap();
        assert!(!sidecar_path(&p).exists(), "no sidecar for JPEG");
        let m = read(&p);
        assert_eq!(m.rating, 5);
        assert_eq!(m.keywords, vec!["a|b"]);
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), old);
        // Still decodes, same pixels.
        let a = image::load_from_memory(&orig).unwrap().to_rgb8();
        let b = image::open(&p).unwrap().to_rgb8();
        assert_eq!(a, b);

        // Second write replaces the segment instead of adding one.
        write(&p, &Edit { rating: Some(2), ..Default::default() }).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let n = bytes.windows(XMP_SIG.len()).filter(|w| *w == XMP_SIG).count();
        assert_eq!(n, 1);
        assert_eq!(read(&p).rating, 2);
        assert_eq!(read(&p).keywords, vec!["a|b"]);
    }

    #[test]
    fn raw_gets_sidecar_and_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("DSC001.ARW");
        std::fs::write(&p, b"raw bytes").unwrap();
        write(&p, &edit_all()).unwrap();
        assert!(tmp.path().join("DSC001.xmp").exists());
        assert_eq!(std::fs::read(&p).unwrap(), b"raw bytes");
        assert_eq!(read(&p).rating, 4);
    }
}

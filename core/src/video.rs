// FFmpeg orchestration.
//
// Three jobs:
//   1. thumb()   - one JPEG frame for the grid (seek ~10%, scaled).
//   2. preview() - short muted low-res H.264 clip that loops on hover,
//                  the trick used by sites playing 40 videos at once.
//   3. playable()- return a path WebView2 can stream:
//                  * mp4/m4v/webm/mov -> original file, zero work;
//                  * mkv and friends  -> stream-copy remux to fragmented
//                    MP4 (a few hundred ms, no re-encode);
//                  * legacy codecs    -> real transcode as last resort
//                    (NVENC on the user's RTX 3080, x264 fallback).
//
// Also used as fallback decoder for exotic images (AVIF/HEIC/JXL/JP2/PSD).

use crate::fnv1a64;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a Command that never flashes a console window on Windows.
/// (The app itself is windowed, so every plain spawn would otherwise
/// open a visible conhost for each ffmpeg/ffprobe call.)
fn quiet_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Container formats WebView2 plays natively when codecs are supported.
const DIRECT_PLAY: &[&str] = &["mp4", "m4v", "webm", "mov"];

/// Codecs WebView2/Chromium can decode (hardware-accelerated on NVIDIA).
const PLAYABLE_CODECS: &[&str] = &["h264", "hevc", "vp8", "vp9", "av1"];

pub struct Ffmpeg {
    pub ffmpeg: String,
    pub ffprobe: String,
    pub cache_dir: PathBuf,
    /// Prefer NVENC for transcodes (set on Windows with an NVIDIA GPU).
    pub use_nvenc: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlayableOut {
    pub path: String,
    /// "direct", "remux" or "transcode" - shown in the UI status bar.
    pub mode: String,
}

impl Ffmpeg {
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            cache_dir: cache_dir.to_path_buf(),
            use_nvenc: cfg!(windows),
        }
    }

    fn key(&self, src: &Path, tag: &str) -> String {
        let meta = std::fs::metadata(src).ok();
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let size = meta.map(|m| m.len()).unwrap_or(0);
        format!(
            "{:016x}",
            fnv1a64(format!("{}|{}|{}|{}", src.display(), mtime, size, tag).as_bytes())
        )
    }

    /// Duration in seconds, via ffprobe.
    pub fn duration(&self, src: &Path) -> Option<f64> {
        let out = quiet_command(&self.ffprobe)
            .args([
                "-v", "error", "-show_entries", "format=duration",
                "-of", "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(src)
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    /// Video codec name of the first video stream, via ffprobe.
    pub fn video_codec(&self, src: &Path) -> Option<String> {
        let out = quiet_command(&self.ffprobe)
            .args([
                "-v", "error", "-select_streams", "v:0",
                "-show_entries", "stream=codec_name",
                "-of", "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(src)
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    /// Extract one frame as a JPEG thumbnail. Seeks to ~10% of the duration
    /// (capped at 30 s) so we skip black intros without slow seeks.
    pub fn thumb(&self, src: &Path, tsize: u32) -> Option<String> {
        let out = self.cache_dir.join("thumbs").join(format!("{}.jpg", self.key(src, &format!("vt{tsize}"))));
        if out.exists() {
            crate::cache::ThumbCache::touch_used(&out); // keep the LRU honest
            return Some(out.to_string_lossy().to_string());
        }
        let seek = self.duration(src).map(|d| (d * 0.10).min(30.0)).unwrap_or(1.0);
        let scale = format!("scale='min({0},iw)':'min({0},ih)':force_original_aspect_ratio=decrease", tsize);
        let status = quiet_command(&self.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{seek:.2}")])
            .arg("-i").arg(src)
            .args(["-frames:v", "1", "-vf", &scale, "-q:v", "4", "-y"])
            .arg(&out)
            .status()
            .ok()?;
        if status.success() && out.exists() {
            Some(out.to_string_lossy().to_string())
        } else {
            None
        }
    }

    /// Decode an exotic image (AVIF/HEIC/JXL/JP2/PSD/SVG...) to a JPEG thumb.
    pub fn image_thumb(&self, src: &Path, tsize: u32) -> Option<String> {
        let out = self.cache_dir.join("thumbs").join(format!("{}.jpg", self.key(src, &format!("it{tsize}"))));
        if out.exists() {
            crate::cache::ThumbCache::touch_used(&out); // keep the LRU honest
            return Some(out.to_string_lossy().to_string());
        }
        let scale = format!("scale='min({0},iw)':'min({0},ih)':force_original_aspect_ratio=decrease", tsize);
        let status = quiet_command(&self.ffmpeg)
            .args(["-v", "error"])
            .arg("-i").arg(src)
            .args(["-frames:v", "1", "-vf", &scale, "-q:v", "4", "-y"])
            .arg(&out)
            .status()
            .ok()?;
        if status.success() && out.exists() {
            Some(out.to_string_lossy().to_string())
        } else {
            None
        }
    }

    /// Build the short looping hover preview: 3 s, 320 px wide, muted,
    /// H.264 baseline, faststart - starts instantly in the grid.
    pub fn preview(&self, src: &Path) -> Option<String> {
        let out = self.cache_dir.join("previews").join(format!("{}.mp4", self.key(src, "pv")));
        if out.exists() {
            crate::cache::ThumbCache::touch_used(&out); // keep the LRU honest
            return Some(out.to_string_lossy().to_string());
        }
        let dur = self.duration(src).unwrap_or(0.0);
        let seek = (dur * 0.10).min(30.0);
        let status = quiet_command(&self.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{seek:.2}")])
            .arg("-i").arg(src)
            .args([
                "-t", "3", "-an",
                "-vf", "scale=320:-2,fps=24,format=yuv420p",
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "28",
                "-profile:v", "baseline", "-movflags", "+faststart", "-y",
            ])
            .arg(&out)
            .status()
            .ok()?;
        if status.success() && out.exists() {
            Some(out.to_string_lossy().to_string())
        } else {
            None
        }
    }

    /// Lossless segment extraction (stream copy), start/end in seconds.
    pub fn cut_video(&self, src: &Path, start: f64, end: f64, out: &Path) -> Result<(), String> {
        let status = quiet_command(&self.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{start:.3}"), "-to", &format!("{end:.3}")])
            .arg("-i").arg(src)
            .args(["-c", "copy", "-avoid_negative_ts", "make_zero", "-y"])
            .arg(out)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() && out.exists() {
            Ok(())
        } else {
            Err("cut failed".into())
        }
    }

    /// Convert (and optionally downscale) an image to the format implied by
    /// `out`'s extension. `max_dim` 0 keeps the size; `quality_pct` 1-100.
    pub fn convert_image(&self, src: &Path, out: &Path, max_dim: u32, quality_pct: u32) -> Result<(), String> {
        let mut c = quiet_command(&self.ffmpeg);
        c.args(["-v", "error"]).arg("-i").arg(src);
        if max_dim > 0 {
            let scale = format!(
                "scale='min({0},iw)':'min({0},ih)':force_original_aspect_ratio=decrease",
                max_dim
            );
            c.args(["-vf", &scale]);
        }
        let ext = out
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let q = quality_pct.clamp(1, 100);
        match ext.as_str() {
            "jpg" | "jpeg" => {
                c.args(["-q:v", &(2 + ((100 - q) * 29) / 100).to_string()]);
            }
            "webp" => {
                c.args(["-quality", &q.to_string()]);
            }
            _ => {}
        }
        c.args(["-frames:v", "1", "-y"]).arg(out);
        let status = c.status().map_err(|e| e.to_string())?;
        if status.success() && out.exists() {
            Ok(())
        } else {
            Err(format!("conversion failed: {}", src.display()))
        }
    }

    /// Return a path the web view can play.
    pub fn playable(&self, src: &Path) -> Option<PlayableOut> {
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let codec = self.video_codec(src).unwrap_or_default();
        let codec_ok = PLAYABLE_CODECS.contains(&codec.as_str());

        // Best case: the browser streams the original file directly.
        if DIRECT_PLAY.contains(&ext.as_str()) && codec_ok {
            return Some(PlayableOut {
                path: src.to_string_lossy().to_string(),
                mode: "direct".into(),
            });
        }

        let out = self.cache_dir.join("play").join(format!("{}.mp4", self.key(src, "pl")));
        if out.exists() {
            crate::cache::ThumbCache::touch_used(&out); // a replay keeps it alive
            return Some(PlayableOut {
                path: out.to_string_lossy().to_string(),
                mode: if codec_ok { "remux".into() } else { "transcode".into() },
            });
        }

        if codec_ok {
            // Stream copy: near-instant, no quality loss (MKV -> MP4).
            // Only the first video and audio tracks are carried over:
            // subtitles, attachments and MKV chapters would land in the MP4
            // as an opaque `bin_data` track, and a web view refuses to play
            // a file containing one.
            let status = quiet_command(&self.ffmpeg)
                .args(["-v", "error"])
                .arg("-i").arg(src)
                .args([
                    "-map", "0:v:0", "-map", "0:a:0?",
                    "-sn", "-dn", "-map_chapters", "-1",
                    "-c:v", "copy", "-c:a", "aac", "-b:a", "192k",
                    "-movflags", "+faststart", "-y",
                ])
                .arg(&out)
                .status()
                .ok()?;
            if status.success() && out.exists() {
                return Some(PlayableOut {
                    path: out.to_string_lossy().to_string(),
                    mode: "remux".into(),
                });
            }
            let _ = std::fs::remove_file(&out);
        }

        // Last resort: transcode (DivX/XviD AVIs, MPEG-2...). NVENC first.
        let encoders: &[&str] = if self.use_nvenc {
            &["h264_nvenc", "libx264"]
        } else {
            &["libx264"]
        };
        for enc in encoders {
            let status = quiet_command(&self.ffmpeg)
                .args(["-v", "error"])
                .arg("-i").arg(src)
                .args([
                    "-map", "0:v:0", "-map", "0:a:0?",
                    "-sn", "-dn", "-map_chapters", "-1",
                    "-c:v", enc, "-preset", "fast",
                    "-c:a", "aac", "-b:a", "192k",
                    "-movflags", "+faststart", "-y",
                ])
                .arg(&out)
                .status()
                .ok();
            if status.map(|s| s.success()).unwrap_or(false) && out.exists() {
                return Some(PlayableOut {
                    path: out.to_string_lossy().to_string(),
                    mode: "transcode".into(),
                });
            }
            let _ = std::fs::remove_file(&out);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a small synthetic test video with ffmpeg itself.
    fn make_video(dir: &Path, name: &str, container_args: &[&str]) -> PathBuf {
        let out = dir.join(name);
        let status = Command::new("ffmpeg")
            .args([
                "-v", "error", "-f", "lavfi", "-i", "testsrc=duration=4:size=640x360:rate=24",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
            ])
            .args(container_args)
            .args(["-y"])
            .arg(&out)
            .status()
            .expect("ffmpeg must run");
        assert!(status.success(), "failed generating {name}");
        out
    }

    #[test]
    fn video_thumb_and_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let ff = Ffmpeg::new(tmp.path());
        std::fs::create_dir_all(tmp.path().join("thumbs")).unwrap();
        std::fs::create_dir_all(tmp.path().join("previews")).unwrap();
        let vid = make_video(tmp.path(), "test.mp4", &["-c:v", "libx264", "-c:a", "aac"]);

        let d = ff.duration(&vid).expect("duration");
        assert!((d - 4.0).abs() < 0.5);

        let thumb = ff.thumb(&vid, 384).expect("thumb");
        assert!(Path::new(&thumb).exists());
        // Second call: cache hit, same path.
        assert_eq!(ff.thumb(&vid, 384).unwrap(), thumb);

        let pv = ff.preview(&vid).expect("preview clip");
        let meta = std::fs::metadata(&pv).unwrap();
        assert!(meta.len() > 1000);
    }

    #[test]
    fn playable_direct_remux_transcode() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ff = Ffmpeg::new(tmp.path());
        ff.use_nvenc = false; // no GPU in the test container
        std::fs::create_dir_all(tmp.path().join("play")).unwrap();

        // MP4/H.264 -> direct.
        let mp4 = make_video(tmp.path(), "a.mp4", &["-c:v", "libx264", "-c:a", "aac"]);
        assert_eq!(ff.playable(&mp4).unwrap().mode, "direct");

        // MKV/H.264 -> remux (stream copy).
        let mkv = make_video(tmp.path(), "b.mkv", &["-c:v", "libx264", "-c:a", "aac"]);
        let p = ff.playable(&mkv).unwrap();
        assert_eq!(p.mode, "remux");
        assert!(p.path.ends_with(".mp4"));

        // AVI/MPEG-4 ASP (DivX-style) -> transcode.
        let avi = make_video(tmp.path(), "c.avi", &["-c:v", "mpeg4", "-c:a", "mp3"]);
        let p = ff.playable(&avi).unwrap();
        assert_eq!(p.mode, "transcode");
        assert!(p.path.ends_with(".mp4"));
    }
}

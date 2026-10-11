//! Loading and saving text files while preserving encoding, BOM and line endings.

use std::path::Path;

use encoding_rs::Encoding;

use crate::{Error, Result, fsutil};

/// Files larger than this are not opened as text (use the hex view instead).
pub const MAX_TEXT_BYTES: u64 = 512 * 1024 * 1024;

/// Line terminator style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub const ALL: [LineEnding; 3] = [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr];

    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF (Unix)",
            LineEnding::CrLf => "CRLF (Windows)",
            LineEnding::Cr => "CR (Classic Mac)",
        }
    }

    /// The platform's native ending, used for new files.
    pub fn native() -> Self {
        if cfg!(windows) { LineEnding::CrLf } else { LineEnding::Lf }
    }
}

/// Text encoding of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Any other WHATWG encoding (Windows-125x, ISO-8859-x, Shift_JIS, GBK, ...).
    Other(&'static Encoding),
}

impl TextEncoding {
    pub fn name(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "UTF-8",
            TextEncoding::Utf16Le => "UTF-16 LE",
            TextEncoding::Utf16Be => "UTF-16 BE",
            TextEncoding::Other(e) => e.name(),
        }
    }

    /// Encodings offered in the "Save as encoding" menu.
    pub fn choices() -> Vec<TextEncoding> {
        let mut v = vec![TextEncoding::Utf8, TextEncoding::Utf16Le, TextEncoding::Utf16Be];
        for e in [
            encoding_rs::WINDOWS_1252,
            encoding_rs::WINDOWS_1250,
            encoding_rs::WINDOWS_1251,
            encoding_rs::ISO_8859_2,
            encoding_rs::ISO_8859_15,
            encoding_rs::KOI8_R,
            encoding_rs::SHIFT_JIS,
            encoding_rs::EUC_KR,
            encoding_rs::GBK,
            encoding_rs::BIG5,
        ] {
            v.push(TextEncoding::Other(e));
        }
        v
    }
}

/// A text file split into lines (without terminators).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFile {
    pub lines: Vec<String>,
    pub encoding: TextEncoding,
    pub bom: bool,
    pub line_ending: LineEnding,
    /// The last line was terminated.
    pub final_newline: bool,
    /// The file mixed different line endings (the dominant one is in `line_ending`).
    pub mixed_endings: bool,
    /// Bytes that could not be decoded were replaced by U+FFFD.
    pub had_decode_errors: bool,
}

impl Default for TextFile {
    fn default() -> Self {
        TextFile {
            lines: Vec::new(),
            encoding: TextEncoding::Utf8,
            bom: false,
            line_ending: LineEnding::native(),
            final_newline: true,
            mixed_endings: false,
            had_decode_errors: false,
        }
    }
}

/// Heuristic: NUL bytes in the first 8 KiB (outside UTF-16 text) mean binary.
pub fn is_probably_binary(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(8192)).unwrap_or(bytes);
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        return false;
    }
    head.contains(&0)
}

impl TextFile {
    /// Load a file from disk.
    pub fn load(path: &Path) -> Result<TextFile> {
        let len = std::fs::metadata(path).map_err(|e| Error::io(path, e))?.len();
        if len > MAX_TEXT_BYTES {
            return Err(Error::TooLarge { path: path.to_path_buf(), size: len, limit: MAX_TEXT_BYTES });
        }
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        Ok(TextFile::from_bytes(&bytes))
    }

    /// Decode bytes, detecting BOM / UTF-8 / legacy encodings.
    pub fn from_bytes(bytes: &[u8]) -> TextFile {
        let (encoding, bom_len) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            (TextEncoding::Utf8, 3)
        } else if bytes.starts_with(&[0xFF, 0xFE]) {
            (TextEncoding::Utf16Le, 2)
        } else if bytes.starts_with(&[0xFE, 0xFF]) {
            (TextEncoding::Utf16Be, 2)
        } else if std::str::from_utf8(bytes).is_ok() {
            (TextEncoding::Utf8, 0)
        } else {
            let mut det = chardetng::EncodingDetector::new();
            det.feed(bytes, true);
            let enc = det.guess(None, true);
            if enc == encoding_rs::UTF_8 { (TextEncoding::Utf8, 0) } else { (TextEncoding::Other(enc), 0) }
        };
        let body = bytes.get(bom_len..).unwrap_or_default();
        let (text, had_errors) = match encoding {
            TextEncoding::Utf8 => {
                let (t, e) = encoding_rs::UTF_8.decode_without_bom_handling(body);
                (t.into_owned(), e)
            }
            TextEncoding::Utf16Le => {
                let (t, e) = encoding_rs::UTF_16LE.decode_without_bom_handling(body);
                (t.into_owned(), e)
            }
            TextEncoding::Utf16Be => {
                let (t, e) = encoding_rs::UTF_16BE.decode_without_bom_handling(body);
                (t.into_owned(), e)
            }
            TextEncoding::Other(enc) => {
                let (t, e) = enc.decode_without_bom_handling(body);
                (t.into_owned(), e)
            }
        };
        let mut f = TextFile::from_text(&text);
        f.encoding = encoding;
        f.bom = bom_len > 0;
        f.had_decode_errors = had_errors;
        f
    }

    /// Split decoded text into lines, recording the dominant line ending.
    pub fn from_text(text: &str) -> TextFile {
        let mut lines = Vec::new();
        let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
        let mut start = 0usize;
        let bytes = text.as_bytes();
        let mut i = 0usize;
        let mut final_newline = false;
        while i < bytes.len() {
            match bytes.get(i) {
                Some(b'\n') => {
                    lines.push(text.get(start..i).unwrap_or_default().to_owned());
                    lf += 1;
                    i += 1;
                    start = i;
                    final_newline = true;
                }
                Some(b'\r') => {
                    lines.push(text.get(start..i).unwrap_or_default().to_owned());
                    if bytes.get(i + 1) == Some(&b'\n') {
                        crlf += 1;
                        i += 2;
                    } else {
                        cr += 1;
                        i += 1;
                    }
                    start = i;
                    final_newline = true;
                }
                _ => {
                    i += 1;
                    final_newline = false;
                }
            }
        }
        if start < bytes.len() {
            lines.push(text.get(start..).unwrap_or_default().to_owned());
            final_newline = false;
        }
        let kinds = [lf, crlf, cr].iter().filter(|&&c| c > 0).count();
        let line_ending = if lf + crlf + cr == 0 {
            LineEnding::native()
        } else if crlf >= lf && crlf >= cr {
            LineEnding::CrLf
        } else if lf >= cr {
            LineEnding::Lf
        } else {
            LineEnding::Cr
        };
        TextFile { lines, line_ending, final_newline, mixed_endings: kinds > 1, ..TextFile::default() }
    }

    /// The text joined with the file's line ending.
    pub fn to_text(&self) -> String {
        let eol = self.line_ending.as_str();
        let mut s = self.lines.join(eol);
        if self.final_newline && !self.lines.is_empty() {
            s.push_str(eol);
        }
        s
    }

    /// Encode to bytes with the file's encoding and BOM.
    pub fn to_bytes(&self) -> Vec<u8> {
        let text = self.to_text();
        let mut out = Vec::with_capacity(text.len() + 4);
        match self.encoding {
            TextEncoding::Utf8 => {
                if self.bom {
                    out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                }
                out.extend_from_slice(text.as_bytes());
            }
            TextEncoding::Utf16Le => {
                if self.bom {
                    out.extend_from_slice(&[0xFF, 0xFE]);
                }
                for u in text.encode_utf16() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
            }
            TextEncoding::Utf16Be => {
                if self.bom {
                    out.extend_from_slice(&[0xFE, 0xFF]);
                }
                for u in text.encode_utf16() {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
            TextEncoding::Other(enc) => {
                let (bytes, _, _) = enc.encode(&text);
                out.extend_from_slice(&bytes);
            }
        }
        out
    }

    /// Save atomically (write a sibling temp file, then rename over the target).
    pub fn save(&self, path: &Path) -> Result<()> {
        fsutil::write_atomic(path, &self.to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_round_trip() {
        for src in ["a\r\nb\r\n", "a\nb", "a\rb\r", "", "\n", "x"] {
            let f = TextFile::from_bytes(src.as_bytes());
            assert_eq!(String::from_utf8(f.to_bytes()).unwrap(), src, "{src:?}");
        }
        let f = TextFile::from_text("a\r\nb\nc\r\n");
        assert_eq!(f.line_ending, LineEnding::CrLf);
        assert!(f.mixed_endings);
        assert_eq!(f.lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn bom_and_utf16_round_trip() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "héllo\r\nwörld".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let f = TextFile::from_bytes(&bytes);
        assert_eq!(f.encoding, TextEncoding::Utf16Le);
        assert_eq!(f.lines, vec!["héllo", "wörld"]);
        assert_eq!(f.to_bytes(), bytes);

        let utf8bom = b"\xEF\xBB\xBFabc\n";
        let f = TextFile::from_bytes(utf8bom);
        assert!(f.bom);
        assert_eq!(f.to_bytes(), utf8bom);
    }

    #[test]
    fn legacy_encoding_detected() {
        // "Größe" in Windows-1252.
        let bytes = b"Gr\xF6\xDFe des Hauses ist sch\xF6n\n";
        let f = TextFile::from_bytes(bytes);
        assert!(matches!(f.encoding, TextEncoding::Other(_)));
        assert!(f.lines[0].starts_with("Größe"));
        assert_eq!(f.to_bytes(), bytes);
    }

    #[test]
    fn binary_detection() {
        assert!(is_probably_binary(b"\x7fELF\0\0\0"));
        assert!(!is_probably_binary(b"plain text"));
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.txt");
        let f = TextFile::from_text("one\r\ntwo\r\n");
        f.save(&p).unwrap();
        assert_eq!(TextFile::load(&p).unwrap(), f);
    }
}

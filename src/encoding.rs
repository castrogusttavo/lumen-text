//! File encodings: how the bytes of a file encode its text. Detected on open, remembered per
//! buffer and used again on save, so a Latin-1 or UTF-16 file is written back the way it came.
//!
//! Detection, in order: a byte order mark (UTF-8, UTF-16 LE/BE); NUL bytes in the first few KB
//! laid out like ASCII in UTF-16 (one NUL per character, all on even or all on odd offsets);
//! other NUL bytes mean a binary file, which is refused with a clear message; then valid UTF-8,
//! which streams straight into the rope with no extra copy (big files stay fast); anything else
//! is read as Windows-1252, the superset of Latin-1 that most "not UTF-8" text files are in.
//! Encodings that bytes cannot reveal (Shift_JIS, KOI8-R...) are reached through the pickers.

use ropey::Rope;
use std::{
    borrow::Cow,
    fs::File,
    io::{self, BufReader, Read, Write},
    path::Path,
};

/// Bytes looked at before deciding how to read a file.
const SNIFF: usize = 8 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// ISO-8859-1: each byte is the code point of the same value. (`encoding_rs` maps this label
    /// to Windows-1252, as browsers do, so it is handled here.)
    Latin1,
    /// Any other encoding `encoding_rs` knows (code pages, Shift_JIS, GBK, Big5...).
    Legacy(&'static encoding_rs::Encoding),
}

/// The encoding of one buffer: what it was read as and is saved as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextEncoding {
    pub kind: Kind,
    /// Write a byte order mark (UTF-8 and UTF-16 only).
    pub bom: bool,
}

impl Default for TextEncoding {
    fn default() -> Self {
        TextEncoding::UTF8
    }
}

/// Encodings offered by the pickers: (id, label). Ids are `encoding_rs` labels (lowercase) plus
/// `utf-8-bom`; [`TextEncoding::from_id`] also takes the `.editorconfig` `charset` values.
pub const CHOICES: &[(&str, &str)] = &[
    ("utf-8", "UTF-8"),
    ("utf-8-bom", "UTF-8 with BOM"),
    ("utf-16le", "UTF-16 LE"),
    ("utf-16be", "UTF-16 BE"),
    ("windows-1252", "Western (Windows-1252)"),
    ("iso-8859-1", "Western (ISO-8859-1)"),
    ("iso-8859-15", "Western (ISO-8859-15)"),
    ("windows-1250", "Central European (Windows-1250)"),
    ("iso-8859-2", "Central European (ISO-8859-2)"),
    ("windows-1251", "Cyrillic (Windows-1251)"),
    ("koi8-r", "Cyrillic (KOI8-R)"),
    ("windows-1253", "Greek (Windows-1253)"),
    ("windows-1254", "Turkish (Windows-1254)"),
    ("shift_jis", "Japanese (Shift_JIS)"),
    ("euc-jp", "Japanese (EUC-JP)"),
    ("gbk", "Chinese Simplified (GBK)"),
    ("big5", "Chinese Traditional (Big5)"),
    ("euc-kr", "Korean (EUC-KR)"),
];

impl TextEncoding {
    pub const UTF8: TextEncoding = TextEncoding { kind: Kind::Utf8, bom: false };

    pub const fn new(kind: Kind, bom: bool) -> TextEncoding {
        TextEncoding { kind, bom }
    }

    /// The encoding an id of [`CHOICES`] (or any `encoding_rs` label, or an `.editorconfig`
    /// `charset` value) names. UTF-16 comes with a BOM; encodings whose encoder keeps state
    /// between characters (ISO-2022-JP) are not offered.
    pub fn from_id(id: &str) -> Option<TextEncoding> {
        let id = id.trim().to_ascii_lowercase();
        Some(match id.as_str() {
            "utf-8" | "utf8" => TextEncoding::UTF8,
            "utf-8-bom" => TextEncoding::new(Kind::Utf8, true),
            "utf-16le" => TextEncoding::new(Kind::Utf16Le, true),
            "utf-16be" => TextEncoding::new(Kind::Utf16Be, true),
            "iso-8859-1" | "latin1" | "latin-1" => TextEncoding::new(Kind::Latin1, false),
            other => {
                let e = encoding_rs::Encoding::for_label_no_replacement(other.as_bytes())?;
                if e == encoding_rs::UTF_8 {
                    TextEncoding::UTF8
                } else if e == encoding_rs::UTF_16LE {
                    TextEncoding::new(Kind::Utf16Le, true)
                } else if e == encoding_rs::UTF_16BE {
                    TextEncoding::new(Kind::Utf16Be, true)
                } else if e == encoding_rs::ISO_2022_JP || e == encoding_rs::X_USER_DEFINED {
                    return None;
                } else {
                    TextEncoding::new(Kind::Legacy(e), false)
                }
            }
        })
    }

    /// Id as in [`CHOICES`] (`utf-16le` with or without BOM).
    pub fn id(&self) -> String {
        match self.kind {
            Kind::Utf8 if self.bom => "utf-8-bom".into(),
            Kind::Utf8 => "utf-8".into(),
            Kind::Utf16Le => "utf-16le".into(),
            Kind::Utf16Be => "utf-16be".into(),
            Kind::Latin1 => "iso-8859-1".into(),
            Kind::Legacy(e) => e.name().to_ascii_lowercase(),
        }
    }

    /// Short name for the status bar: "UTF-8", "UTF-8 with BOM", "UTF-16 LE", "Windows-1252"...
    pub fn short(&self) -> String {
        match self.kind {
            Kind::Utf8 if self.bom => "UTF-8 with BOM".into(),
            Kind::Utf8 => "UTF-8".into(),
            Kind::Utf16Le => "UTF-16 LE".into(),
            Kind::Utf16Be => "UTF-16 BE".into(),
            Kind::Latin1 => "ISO-8859-1".into(),
            Kind::Legacy(e) => match e.name() {
                n if n.starts_with("windows-") => format!("Windows-{}", &n["windows-".len()..]),
                n => n.to_string(),
            },
        }
    }

    /// Picker label ("Western (Windows-1252)"), or the short name for encodings not listed.
    pub fn label(&self) -> String {
        let id = self.id();
        CHOICES.iter().find(|(i, _)| *i == id).map_or_else(|| self.short(), |(_, l)| l.to_string())
    }

    /// Can the bytes not reveal this encoding? Then a reload keeps it instead of detecting again.
    pub fn sticky(&self) -> bool {
        matches!(self.kind, Kind::Latin1 | Kind::Legacy(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sniff {
    /// Certain: a byte order mark of that many bytes (0 for UTF-16 told by its NUL pattern).
    Known(TextEncoding, usize),
    /// No BOM and no NUL: UTF-8 if it validates, else the legacy fallback.
    Utf8Maybe,
    Binary,
}

fn sniff(head: &[u8]) -> Sniff {
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Sniff::Known(TextEncoding::new(Kind::Utf8, true), 3);
    }
    if head.starts_with(&[0xFF, 0xFE]) {
        return Sniff::Known(TextEncoding::new(Kind::Utf16Le, true), 2);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Sniff::Known(TextEncoding::new(Kind::Utf16Be, true), 2);
    }
    if !head.contains(&0) {
        return Sniff::Utf8Maybe;
    }
    match utf16_guess(head) {
        Some(k) => Sniff::Known(TextEncoding::new(k, false), 0),
        None => Sniff::Binary,
    }
}

/// UTF-16 without a BOM, told by where the NUL bytes are: mostly-ASCII text has a NUL high byte
/// in (nearly) every code unit, on odd offsets in little endian and even ones in big endian.
fn utf16_guess(head: &[u8]) -> Option<Kind> {
    let units = head.len() / 2;
    if units == 0 {
        return None;
    }
    let (mut even, mut odd) = (0usize, 0usize);
    for (i, b) in head[..units * 2].iter().enumerate() {
        if *b == 0 {
            if i % 2 == 0 {
                even += 1;
            } else {
                odd += 1;
            }
        }
    }
    let many = |n: usize| n * 10 >= units * 3; // at least 30% of the code units
    let few = |n: usize| n * 50 <= units; // at most 2%
    if many(odd) && few(even) {
        Some(Kind::Utf16Le)
    } else if many(even) && few(odd) {
        Some(Kind::Utf16Be)
    } else {
        None
    }
}

/// Binary data rather than text in some 8-bit encoding: NUL bytes, or more than 5% control
/// characters that text does not use (tab, line breaks, form feed, escape and backspace do occur).
fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    if sample.contains(&0) {
        return true;
    }
    let ctrl = sample
        .iter()
        .filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B | 0x08)) || b == 0x7F)
        .count();
    ctrl * 20 > sample.len()
}

fn binary_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "this looks like a binary file, not text")
}

/// Read `path` as text: decoded as `forced`, or with the detected encoding when `None` (see the
/// module docs). Valid UTF-8 is streamed into the rope without an intermediate copy.
pub fn read_file(path: &Path, forced: Option<TextEncoding>) -> io::Result<(Rope, TextEncoding)> {
    let mut f = File::open(path)?;
    let mut head = Vec::with_capacity(SNIFF);
    f.by_ref().take(SNIFF as u64).read_to_end(&mut head)?;
    let enc = match forced {
        Some(e) => e,
        None => match sniff(&head) {
            Sniff::Binary => return Err(binary_error()),
            Sniff::Known(e, bom) if e.kind == Kind::Utf8 => {
                // UTF-8 with a BOM: stream the rest; invalid bytes are replaced further down.
                match Rope::from_reader(BufReader::new(io::Cursor::new(&head[bom..]).chain(&mut f))) {
                    Ok(rope) => return Ok((rope, e)),
                    Err(err) if err.kind() == io::ErrorKind::InvalidData => e,
                    Err(err) => return Err(err),
                }
            }
            Sniff::Known(e, _) => e,
            Sniff::Utf8Maybe => match Rope::from_reader(BufReader::new(io::Cursor::new(&head[..]).chain(&mut f))) {
                Ok(rope) => return Ok((rope, TextEncoding::UTF8)),
                Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                    TextEncoding::new(Kind::Legacy(encoding_rs::WINDOWS_1252), false)
                }
                Err(err) => return Err(err),
            },
        },
    };
    // Every other case decodes the whole file at once (read again: the stream above may have
    // consumed part of it).
    let bytes = std::fs::read(path)?;
    if forced.is_none() && enc.sticky() && looks_binary(&bytes) {
        return Err(binary_error());
    }
    Ok((Rope::from_str(&decode(&bytes, enc)), enc))
}

/// The text `bytes` encode in `enc` (a leading BOM of that encoding is dropped; invalid
/// sequences become U+FFFD).
pub fn decode(bytes: &[u8], enc: TextEncoding) -> String {
    match enc.kind {
        Kind::Utf8 => {
            let b = bytes.strip_prefix(&[0xEFu8, 0xBB, 0xBF][..]).unwrap_or(bytes);
            String::from_utf8_lossy(b).into_owned()
        }
        Kind::Utf16Le | Kind::Utf16Be => {
            let le = enc.kind == Kind::Utf16Le;
            let bom: [u8; 2] = if le { [0xFF, 0xFE] } else { [0xFE, 0xFF] };
            let b = bytes.strip_prefix(&bom[..]).unwrap_or(bytes);
            let units = b
                .chunks_exact(2)
                .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
            let mut s: String = char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect();
            if b.len() % 2 == 1 {
                s.push(char::REPLACEMENT_CHARACTER);
            }
            s
        }
        Kind::Latin1 => bytes.iter().map(|&b| b as char).collect(),
        Kind::Legacy(e) => e.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

/// `chunk` in a single-byte or legacy multi-byte encoding; `Err(byte offset)` of the first
/// character it cannot represent. (Rope chunks hold whole characters, and these encoders keep no
/// state between characters, so chunks can be encoded one by one.)
fn encode_chunk(chunk: &str, kind: Kind) -> Result<Cow<'_, [u8]>, usize> {
    match kind {
        Kind::Latin1 => {
            if chunk.is_ascii() {
                return Ok(Cow::Borrowed(chunk.as_bytes()));
            }
            let mut out = Vec::with_capacity(chunk.len());
            for (i, c) in chunk.char_indices() {
                let n = c as u32;
                if n > 0xFF {
                    return Err(i);
                }
                out.push(n as u8);
            }
            Ok(Cow::Owned(out))
        }
        Kind::Legacy(e) => {
            let (bytes, _, unmappable) = e.encode(chunk);
            if !unmappable {
                return Ok(bytes);
            }
            let mut buf = [0u8; 4];
            let bad = chunk.char_indices().find(|(_, c)| e.encode(c.encode_utf8(&mut buf)).2).map_or(0, |(i, _)| i);
            Err(bad)
        }
        Kind::Utf8 | Kind::Utf16Le | Kind::Utf16Be => Ok(Cow::Borrowed(chunk.as_bytes())),
    }
}

/// Write `rope` encoded as `enc` (with its BOM when it has one). Fails, before anything is
/// written for that chunk, on a character the encoding cannot represent.
pub fn write_rope<W: Write>(rope: &Rope, enc: TextEncoding, w: &mut W) -> io::Result<()> {
    match enc.kind {
        Kind::Utf8 => {
            if enc.bom {
                w.write_all(&[0xEF, 0xBB, 0xBF])?;
            }
            rope.write_to(&mut *w)
        }
        Kind::Utf16Le | Kind::Utf16Be => {
            let le = enc.kind == Kind::Utf16Le;
            if enc.bom {
                let bom: [u8; 2] = if le { [0xFF, 0xFE] } else { [0xFE, 0xFF] };
                w.write_all(&bom)?;
            }
            let mut out: Vec<u8> = Vec::with_capacity(16 * 1024);
            for chunk in rope.chunks() {
                for u in chunk.encode_utf16() {
                    out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
                }
                if out.len() >= 8 * 1024 {
                    w.write_all(&out)?;
                    out.clear();
                }
            }
            w.write_all(&out)
        }
        Kind::Latin1 | Kind::Legacy(_) => {
            let mut lines = 0usize;
            for chunk in rope.chunks() {
                match encode_chunk(chunk, enc.kind) {
                    Ok(bytes) => w.write_all(&bytes)?,
                    Err(at) => {
                        let line = lines + chunk[..at].matches('\n').count() + 1;
                        let c = chunk[at..].chars().next().unwrap_or('?');
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "{} cannot represent “{c}” (line {line}); use “Save with encoding…” to pick UTF-8",
                                enc.short()
                            ),
                        ));
                    }
                }
                lines += chunk.matches('\n').count();
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("lumen-enc-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn roundtrip(bytes: &[u8]) -> (String, TextEncoding, Vec<u8>) {
        let d = tmp("rt");
        let p = d.join("f.txt");
        std::fs::write(&p, bytes).unwrap();
        let (rope, enc) = read_file(&p, None).unwrap();
        let mut out = Vec::new();
        write_rope(&rope, enc, &mut out).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        (rope.to_string(), enc, out)
    }

    #[test]
    fn utf8_and_boms_are_detected_and_written_back_byte_for_byte() {
        let (t, e, out) = roundtrip("olá\n".as_bytes());
        assert_eq!((t.as_str(), e), ("olá\n", TextEncoding::UTF8));
        assert_eq!(out, "olá\n".as_bytes());
        let with_bom = [&[0xEF, 0xBB, 0xBF][..], "olá\n".as_bytes()].concat();
        let (t, e, out) = roundtrip(&with_bom);
        assert_eq!((t.as_str(), e.id().as_str()), ("olá\n", "utf-8-bom"), "the BOM is not part of the text");
        assert_eq!(out, with_bom);
        let le = [0xFF, 0xFE, b'h', 0, 0xE9, 0, b'\n', 0];
        let (t, e, out) = roundtrip(&le);
        assert_eq!((t.as_str(), e), ("hé\n", TextEncoding::new(Kind::Utf16Le, true)));
        assert_eq!(out, le);
        let be = [0xFE, 0xFF, 0, b'h', 0, 0xE9, 0xD8, 0x3D, 0xDE, 0x00];
        let (t, e, out) = roundtrip(&be);
        assert_eq!((t.as_str(), e.kind), ("hé😀", Kind::Utf16Be), "surrogate pairs decode");
        assert_eq!(out, be);
    }

    #[test]
    fn utf16_without_bom_is_told_by_its_nul_pattern() {
        let text = "fn main() {}\r\n";
        let le: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let (t, e, out) = roundtrip(&le);
        assert_eq!((t.as_str(), e), (text, TextEncoding::new(Kind::Utf16Le, false)));
        assert_eq!(out, le, "saved again without a BOM");
        let be: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let (t, e, _) = roundtrip(&be);
        assert_eq!((t.as_str(), e.kind, e.bom), (text, Kind::Utf16Be, false));
    }

    #[test]
    fn invalid_utf8_falls_back_to_windows_1252_and_saves_the_same_bytes() {
        // "café – ok" in Windows-1252 (0xE9 é, 0x96 en dash).
        let bytes = b"caf\xE9 \x96 ok\n";
        let (t, e, out) = roundtrip(bytes);
        assert_eq!(t, "café – ok\n");
        assert_eq!(e.id(), "windows-1252");
        assert_eq!(e.short(), "Windows-1252");
        assert_eq!(out, bytes);
    }

    #[test]
    fn binary_files_are_refused_with_a_clear_message() {
        let d = tmp("bin");
        let p = d.join("x.png");
        std::fs::write(&p, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x01\0\0\0\x01\0\x08\x06\0\0\0\x5c\x72\xa8").unwrap();
        let err = read_file(&p, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("binary"), "{err}");
        // Not UTF-8 and full of control bytes: binary too, not mojibake.
        let noisy: Vec<u8> = (0..400u32).map(|i| [0x01, 0x02, 0xFF, 0x03, 0x90][i as usize % 5]).collect();
        std::fs::write(&p, &noisy).unwrap();
        assert!(read_file(&p, None).is_err());
        // An empty file is plain UTF-8.
        std::fs::write(&p, b"").unwrap();
        let (r, e) = read_file(&p, None).unwrap();
        assert_eq!((r.len_chars(), e), (0, TextEncoding::UTF8));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn forced_encodings_decode_and_encode() {
        let d = tmp("forced");
        let p = d.join("f.txt");
        std::fs::write(&p, b"\x80 \xE9").unwrap();
        let latin1 = TextEncoding::from_id("latin1").unwrap();
        let (r, _) = read_file(&p, Some(latin1)).unwrap();
        assert_eq!(r.to_string(), "\u{80} é", "ISO-8859-1 maps 0x80 to U+0080, unlike Windows-1252");
        let w1252 = TextEncoding::from_id("windows-1252").unwrap();
        let (r, _) = read_file(&p, Some(w1252)).unwrap();
        assert_eq!(r.to_string(), "€ é");
        let sjis = TextEncoding::from_id("shift_jis").unwrap();
        let mut out = Vec::new();
        write_rope(&Rope::from_str("日本"), sjis, &mut out).unwrap();
        assert_eq!(out, b"\x93\xFA\x96\x7B");
        std::fs::write(&p, &out).unwrap();
        assert_eq!(read_file(&p, Some(sjis)).unwrap().0.to_string(), "日本");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unrepresentable_characters_fail_the_save_with_the_line() {
        let mut out = Vec::new();
        let err = write_rope(&Rope::from_str("a\nb\nnão 日\n"), TextEncoding::from_id("latin1").unwrap(), &mut out)
            .unwrap_err();
        assert!(err.to_string().contains("“日” (line 3)"), "{err}");
        let err =
            write_rope(&Rope::from_str("x 😀"), TextEncoding::from_id("windows-1252").unwrap(), &mut out).unwrap_err();
        assert!(err.to_string().starts_with("Windows-1252 cannot represent “😀” (line 1)"), "{err}");
    }

    #[test]
    fn ids_labels_and_editorconfig_charsets() {
        for (id, _) in CHOICES {
            let e = TextEncoding::from_id(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(e.id(), *id, "id round trip");
            assert!(!e.label().is_empty() && !e.short().is_empty());
        }
        for cs in ["latin1", "utf-8", "utf-8-bom", "utf-16be", "utf-16le"] {
            assert!(TextEncoding::from_id(cs).is_some(), "{cs}");
        }
        assert_eq!(TextEncoding::from_id("UTF-8"), Some(TextEncoding::UTF8));
        assert_eq!(TextEncoding::from_id("latin1").map(|e| e.short()).as_deref(), Some("ISO-8859-1"));
        assert_eq!(TextEncoding::from_id("iso-2022-jp"), None);
        assert_eq!(TextEncoding::from_id("bogus"), None);
        assert!(TextEncoding::from_id("koi8-r").unwrap().sticky() && !TextEncoding::UTF8.sticky());
    }
}

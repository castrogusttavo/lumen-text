//! Packed icon masks: one compressed blob holding many `.a8` files, used to embed the licensed
//! HugeIcons Pro masks inside the executable (see `build.rs`). `build.rs` includes this file
//! too, so it must stay free of crate-internal dependencies.
//!
//! Raw layout (then deflate-compressed): `u32 LE count`, then per entry `u16 LE name length`,
//! the UTF-8 name (e.g. `hugeicons/check`), `u32 LE data length` and the `.a8` bytes.
#![allow(dead_code)]

use std::collections::HashMap;

/// Pack `(name, a8 bytes)` entries into one compressed blob.
pub fn pack(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, data) in entries {
        raw.extend_from_slice(&(name.len() as u16).to_le_bytes());
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(&(data.len() as u32).to_le_bytes());
        raw.extend_from_slice(data);
    }
    miniz_oxide::deflate::compress_to_vec(&raw, 9)
}

/// A decompressed blob with a name index.
pub struct Pack {
    raw: Vec<u8>,
    index: HashMap<String, (usize, usize)>,
}

impl Pack {
    pub fn parse(blob: &[u8]) -> Option<Pack> {
        let raw = miniz_oxide::inflate::decompress_to_vec(blob).ok()?;
        let mut index = HashMap::new();
        let n = u32::from_le_bytes(raw.get(0..4)?.try_into().ok()?) as usize;
        let mut at = 4;
        for _ in 0..n {
            let nl = u16::from_le_bytes(raw.get(at..at + 2)?.try_into().ok()?) as usize;
            let name = std::str::from_utf8(raw.get(at + 2..at + 2 + nl)?).ok()?.to_string();
            at += 2 + nl;
            let dl = u32::from_le_bytes(raw.get(at..at + 4)?.try_into().ok()?) as usize;
            at += 4;
            raw.get(at..at + dl)?;
            index.insert(name, (at, dl));
            at += dl;
        }
        Some(Pack { raw, index })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.index.get(name).map(|&(at, len)| &self.raw[at..at + len])
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }
}

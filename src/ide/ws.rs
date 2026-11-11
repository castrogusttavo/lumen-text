//! The little of RFC 6455 the IDE bridge needs: the opening handshake's accept key (SHA-1 +
//! base64, both written here to stay dependency-free), and frames — masking, 7 / 16 / 64-bit
//! lengths, fragmented messages, ping / pong and close. No extensions (no permessage-deflate), so
//! any RSV bit is a protocol error. Sizes are capped: a frame or message over the cap is refused.

use std::io::{self, Read};

pub const OP_CONT: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut s = key.trim().as_bytes().to_vec();
    s.extend_from_slice(GUID.as_bytes());
    base64(&sha1(&s))
}

/// SHA-1 (FIPS 180-4). Only used for the handshake, where it is what the RFC asks for.
#[allow(clippy::needless_range_loop)]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((*c.get(1).unwrap_or(&0) as u32) << 8) | (*c.get(2).unwrap_or(&0) as u32);
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

/// One frame off the wire (payload already unmasked).
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub op: u8,
    pub payload: Vec<u8>,
}

/// A frame: FIN set, masked with `mask` when given (clients mask, servers do not).
pub fn encode(op: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    encode_frame(true, op, payload, mask)
}

pub fn encode_frame(fin: bool, op: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push((if fin { 0x80 } else { 0 }) | (op & 0x0F));
    let m = if mask.is_some() { 0x80 } else { 0 };
    let n = payload.len();
    if n < 126 {
        out.push(m | n as u8);
    } else if n <= u16::MAX as usize {
        out.push(m | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(m | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    match mask {
        Some(k) => {
            out.extend_from_slice(&k);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ k[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Read one frame. `masked`: the peer must mask (a server reading a client); `max`: payload cap.
pub fn read_frame(r: &mut impl Read, max: usize, masked: bool) -> io::Result<Frame> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h)?;
    let fin = h[0] & 0x80 != 0;
    if h[0] & 0x70 != 0 {
        return Err(bad("reserved bits set (no extension was negotiated)"));
    }
    let op = h[0] & 0x0F;
    let is_mask = h[1] & 0x80 != 0;
    if masked && !is_mask {
        return Err(bad("client frames must be masked"));
    }
    let len = match h[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u16::from_be_bytes(b) as u64
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            u64::from_be_bytes(b)
        }
        n => n as u64,
    };
    if op >= 0x8 && (len > 125 || !fin) {
        return Err(bad("control frames are short and never fragmented"));
    }
    if !matches!(op, OP_CONT | OP_TEXT | OP_BINARY | OP_CLOSE | OP_PING | OP_PONG) {
        return Err(bad("unknown opcode"));
    }
    if len > max as u64 {
        return Err(bad("frame too large"));
    }
    let mut key = [0u8; 4];
    if is_mask {
        r.read_exact(&mut key)?;
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    if is_mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
    }
    Ok(Frame { fin, op, payload })
}

/// A whole message (fragments joined), or a control frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Msg {
    Text(String),
    /// Binary data (the bridge has no use for it).
    Binary,
    Ping(Vec<u8>),
    Pong,
    /// Status code, when the peer sent one.
    Close(Option<u16>),
}

/// Joins fragmented data frames; control frames pass through between fragments.
pub struct Assembler {
    op: Option<u8>,
    buf: Vec<u8>,
    max: usize,
}

impl Assembler {
    pub fn new(max: usize) -> Self {
        Assembler { op: None, buf: Vec::new(), max }
    }

    /// Feed a frame; a message comes out when one is complete.
    pub fn push(&mut self, f: Frame) -> io::Result<Option<Msg>> {
        match f.op {
            OP_PING => return Ok(Some(Msg::Ping(f.payload))),
            OP_PONG => return Ok(Some(Msg::Pong)),
            OP_CLOSE => {
                let code = (f.payload.len() >= 2).then(|| u16::from_be_bytes([f.payload[0], f.payload[1]]));
                return Ok(Some(Msg::Close(code)));
            }
            OP_CONT => {
                if self.op.is_none() {
                    return Err(bad("continuation without a message"));
                }
            }
            _ => {
                if self.op.is_some() {
                    return Err(bad("new message inside a fragmented one"));
                }
                self.op = Some(f.op);
            }
        }
        if self.buf.len() + f.payload.len() > self.max {
            return Err(bad("message too large"));
        }
        self.buf.extend_from_slice(&f.payload);
        if !f.fin {
            return Ok(None);
        }
        let data = std::mem::take(&mut self.buf);
        Ok(Some(match self.op.take() {
            Some(OP_TEXT) => Msg::Text(String::from_utf8(data).map_err(|_| bad("text is not UTF-8"))?),
            _ => Msg::Binary,
        }))
    }
}

/// Read until a whole message (or a control frame) arrives.
pub fn read_message(r: &mut impl Read, asm: &mut Assembler, masked: bool) -> io::Result<Msg> {
    loop {
        let f = read_frame(r, asm.max, masked)?;
        if let Some(m) = asm.push(f)? {
            return Ok(m);
        }
    }
}

/// Close frame payload with a status code.
pub fn close_payload(code: u16) -> Vec<u8> {
    code.to_be_bytes().to_vec()
}

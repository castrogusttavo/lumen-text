//! JSON-RPC 2.0 over stdio with `Content-Length` framing.

use std::io::{self, BufRead, Read, Write};

use serde_json::{Value, json};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const SERVER_NOT_INITIALIZED: i64 = -32002;
pub const REQUEST_CANCELLED: i64 = -32800;

/// Largest body accepted (bytes). A bigger message is read and thrown away, and answered as an
/// invalid request, instead of allocating whatever a bogus `Content-Length` asks for.
pub const MAX_MESSAGE: usize = 512 * 1024 * 1024;

/// Longest header line looked at; a longer one is not a header.
const MAX_HEADER_LINE: u64 = 16 * 1024;

/// Read one framed message. `Ok(None)` is a clean end of input. A body that is not valid JSON
/// (or not valid UTF-8, or over [`MAX_MESSAGE`]) comes back as `Value::Null` so the server can
/// answer with an error and carry on. Header lines are read as bytes: a header that is not UTF-8
/// is skipped instead of ending the session.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if r.by_ref().take(MAX_HEADER_LINE).read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end();
        if text.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = text.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let len = len.unwrap_or(0);
    if len > MAX_MESSAGE {
        let skipped = io::copy(&mut r.by_ref().take(len as u64), &mut io::sink())?;
        if skipped < len as u64 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        return Ok(Some(Value::Null));
    }
    // Grown as the bytes arrive, so a lying `Content-Length` costs nothing.
    let mut buf = Vec::new();
    r.by_ref().take(len as u64).read_to_end(&mut buf)?;
    if buf.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(serde_json::from_slice(&buf).unwrap_or(Value::Null)))
}

pub fn write_message(w: &mut impl Write, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut out = Vec::new();
        write_message(&mut out, &json!({"a": 1})).unwrap();
        write_message(&mut out, &json!({"b": "ç"})).unwrap();
        let mut r = io::BufReader::new(&out[..]);
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({"a": 1})));
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({"b": "ç"})));
        assert_eq!(read_message(&mut r).unwrap(), None);
    }

    #[test]
    fn bad_json_is_null() {
        let raw = b"Content-Length: 3\r\n\r\n{{{";
        let mut r = io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap(), Some(Value::Null));
    }

    #[test]
    fn non_utf8_json_and_headers_do_not_end_the_session() {
        let mut raw = b"X-Junk: \xff\xfe\r\nContent-Length: 3\r\n\r\n\"\xff\"".to_vec();
        raw.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
        let mut r = io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap(), Some(Value::Null));
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({})));
    }

    #[test]
    fn lying_content_length_is_an_error_not_an_allocation() {
        let raw = b"Content-Length: 4000000000\r\n\r\n{}";
        let mut r = io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let raw = b"Content-Length: 99999999999\r\n\r\n{}";
        let mut r = io::BufReader::new(&raw[..]);
        assert!(read_message(&mut r).is_err());
    }

    #[test]
    fn header_case_and_extra_headers() {
        let raw = b"content-type: x\r\ncontent-length: 2\r\n\r\n{}";
        let mut r = io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({})));
    }
}

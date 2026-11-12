//! JSON-RPC 2.0 over stdio with `Content-Length` framing.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

/// Read one framed message. `Ok(None)` is a clean end of input. A body that is not valid JSON
/// comes back as `Value::Null` so the server can answer with an error and carry on.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let mut buf = vec![0u8; len.unwrap_or(0)];
    r.read_exact(&mut buf)?;
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
    fn header_case_and_extra_headers() {
        let raw = b"content-type: x\r\ncontent-length: 2\r\n\r\n{}";
        let mut r = io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap(), Some(json!({})));
    }
}

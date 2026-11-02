//! Example RAG2 plugin: a `document_transform` that replaces e-mail addresses with
//! `[redacted email]` before anything is stored or indexed. Config `{"drop_if_contains": "..."}`
//! drops documents containing that marker.
//!
//! Build: `cargo build --release --target wasm32-unknown-unknown`
//! Output: `target/wasm32-unknown-unknown/release/redact_emails.wasm`

use serde_json::{Value, json};

const ABI_VERSION: i32 = 1;

/// Return a buffer to the host as `(ptr << 32) | len`, leaking it (the instance is discarded
/// after each call, so nothing accumulates).
fn give(bytes: Vec<u8>) -> i64 {
    let b = bytes.into_boxed_slice();
    let (ptr, len) = (b.as_ptr() as i64, b.len() as i64);
    std::mem::forget(b);
    (ptr << 32) | len
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: i32) -> i32 {
    let mut v = Vec::<u8>::with_capacity(len.max(0) as usize);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn rag2_abi_version() -> i32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn rag2_manifest() -> i64 {
    give(json!({ "name": "redact-emails", "version": "1.0.0", "hooks": ["document_transform"] }).to_string().into_bytes())
}

fn is_email_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

/// Replace `local@domain.tld` spans.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = out.len();
            for (pos, c) in out.char_indices().rev() {
                if is_email_char(c) { start = pos } else { break }
            }
            let mut j = i + 1;
            while j < chars.len() && is_email_char(chars[j]) {
                j += 1;
            }
            let domain: String = chars[i + 1..j].iter().collect();
            let domain = domain.trim_end_matches('.');
            if start < out.len() && domain.contains('.') {
                out.truncate(start);
                out.push_str("[redacted email]");
                i += 1 + domain.chars().count();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn handle(envelope: &Value) -> Value {
    if envelope["hook"] != "document_transform" {
        return json!({ "error": "unsupported hook" });
    }
    let mut doc = envelope["input"].clone();
    let text = doc["text"].as_str().unwrap_or("").to_string();
    if let Some(marker) = envelope["config"]["drop_if_contains"].as_str()
        && !marker.is_empty()
        && text.contains(marker)
    {
        return Value::Null;
    }
    doc["text"] = Value::String(redact(&text));
    doc
}

#[unsafe(no_mangle)]
pub extern "C" fn call(ptr: i32, len: i32) -> i64 {
    // SAFETY: the host wrote `len` bytes at `ptr`, a buffer it obtained from `alloc`.
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let out = match serde_json::from_slice::<Value>(input) {
        Ok(env) => handle(&env),
        Err(e) => json!({ "error": format!("bad envelope: {e}") }),
    };
    give(out.to_string().into_bytes())
}

#[cfg(test)]
mod tests {
    #[test]
    fn redacts_addresses_and_keeps_the_rest() {
        assert_eq!(super::redact("Mail ana.b+x@corp.example.com or bob@x.io."), "Mail [redacted email] or [redacted email].");
        assert_eq!(super::redact("price @ 5 and a@b"), "price @ 5 and a@b");
    }
}

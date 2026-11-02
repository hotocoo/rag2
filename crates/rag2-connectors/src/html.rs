//! HTML to readable text without an HTML engine: drop `script`/`style`/`noscript`/`head`
//! content, turn block elements into line breaks, decode the common entities, collapse spaces.
//! Good enough for indexing; not a renderer.

const SKIP: [&str; 4] = ["script", "style", "noscript", "head"];
const BLOCK: [&str; 16] = [
    "p", "div", "br", "li", "ul", "ol", "tr", "table", "section", "article", "h1", "h2", "h3",
    "h4", "h5", "h6",
];

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Readable text of an HTML document.
pub fn to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut skip_depth = 0usize;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        if skip_depth == 0 {
            out.push_str(&decode(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        if tag.starts_with('!') {
            continue;
        }
        let name = tag_name(tag);
        let closing = tag.starts_with('/');
        if SKIP.contains(&name.as_str()) {
            if closing {
                skip_depth = skip_depth.saturating_sub(1);
            } else if !tag.ends_with('/') {
                skip_depth += 1;
            }
            continue;
        }
        if skip_depth == 0 && BLOCK.contains(&name.as_str()) {
            out.push('\n');
        }
    }
    if skip_depth == 0 {
        out.push_str(&decode(rest));
    }
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `<title>` text, if any.
pub fn title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')? + start + 1;
    let close = lower[open_end..].find("</title>")? + open_end;
    let t = decode(&html[open_end..close])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_scripts_and_decodes_entities() {
        let h = "<html><head><title>T &amp; U</title><script>var x = '<p>';</script></head><body><h1>Hi</h1><p>a &lt; b&nbsp;&#169;</p><!-- c --><ul><li>one</li><li>two</li></ul></body></html>";
        assert_eq!(to_text(h), "Hi\na < b ©\none\ntwo");
        assert_eq!(title(h).as_deref(), Some("T & U"));
        assert_eq!(to_text("plain &unknown; text"), "plain &unknown; text");
    }
}

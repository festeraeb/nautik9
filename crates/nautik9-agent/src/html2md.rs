//! Page HTML to Markdown for `web_fetch` (Jade 2.20.0 intent: title, headings, paragraphs,
//! lists, links, code, tables; no scripts, styles, menus or hidden parts). A small tag scanner,
//! not a full HTML parser: unknown tags are dropped and their text kept.

/// Elements whose whole content is dropped.
const SKIP: &[&str] = &["script", "style", "noscript", "nav", "header", "footer", "aside", "svg", "form", "iframe", "template", "button", "select"];

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let Some(e) = end else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..e];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let low = tag.to_ascii_lowercase();
    let key = format!("{name}=");
    let mut from = 0;
    while let Some(p) = low[from..].find(&key) {
        let i = from + p;
        let ok_before = i == 0 || low.as_bytes()[i - 1].is_ascii_whitespace();
        let v = &tag[i + key.len()..];
        if ok_before {
            let val = match v.chars().next() {
                Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
                _ => v.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or(""),
            };
            return Some(decode_entities(val));
        }
        from = i + key.len();
    }
    None
}

/// (title, markdown) of a page. `base` resolves relative links (scheme://host).
pub fn html_to_markdown(html: &str, base: &str) -> (Option<String>, String) {
    let mut out = String::new();
    let mut title: Option<String> = None;
    let mut skip_depth: Vec<String> = Vec::new();
    let mut in_pre = false;
    let mut in_title = false;
    let mut link: Option<String> = None;
    let mut list_depth: usize = 0;
    let mut cell_in_row = 0usize;
    let mut i = 0;
    let b = html.as_bytes();
    let base_root = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");

    let newline = |out: &mut String, n: usize| {
        let have = out.chars().rev().take_while(|&c| c == '\n').count();
        if !out.is_empty() {
            for _ in have..n {
                out.push('\n');
            }
        }
    };

    while i < b.len() {
        if b[i] == b'<' {
            if html[i..].starts_with("<!--") {
                i = html[i..].find("-->").map(|e| i + e + 3).unwrap_or(b.len());
                continue;
            }
            let Some(e) = html[i..].find('>') else { break };
            let tag = &html[i + 1..i + e];
            i += e + 1;
            let closing = tag.starts_with('/');
            let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
            if let Some(top) = skip_depth.last() {
                if closing && &name == top {
                    skip_depth.pop();
                } else if !closing && SKIP.contains(&name.as_str()) && !tag.ends_with('/') {
                    skip_depth.push(name);
                }
                continue;
            }
            let bare_hidden = tag.split_whitespace().skip(1).any(|t| t.trim_end_matches('/').eq_ignore_ascii_case("hidden"));
            let hidden = !closing && (bare_hidden || attr(tag, "hidden").is_some() || attr(tag, "aria-hidden").is_some_and(|v| v == "true") || attr(tag, "style").is_some_and(|s| s.replace(' ', "").contains("display:none")));
            if !closing && (SKIP.contains(&name.as_str()) || hidden) && !tag.ends_with('/') {
                skip_depth.push(name);
                continue;
            }
            match (name.as_str(), closing) {
                ("title", false) => in_title = true,
                ("title", true) => in_title = false,
                ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                    newline(&mut out, 2);
                    out.push_str(&"#".repeat(name[1..].parse::<usize>().unwrap_or(1)));
                    out.push(' ');
                }
                ("h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section" | "article" | "blockquote" | "table", true) => newline(&mut out, 2),
                ("p" | "div" | "section" | "article" | "table", false) => newline(&mut out, 2),
                ("blockquote", false) => {
                    newline(&mut out, 2);
                    out.push_str("> ");
                }
                ("br", _) => out.push('\n'),
                ("ul" | "ol", false) => {
                    list_depth += 1;
                    newline(&mut out, 1);
                }
                ("ul" | "ol", true) => {
                    list_depth = list_depth.saturating_sub(1);
                    newline(&mut out, 1);
                }
                ("li", false) => {
                    newline(&mut out, 1);
                    out.push_str(&"  ".repeat(list_depth.saturating_sub(1)));
                    out.push_str("- ");
                }
                ("tr", false) => {
                    newline(&mut out, 1);
                    out.push('|');
                    cell_in_row = 0;
                }
                ("td" | "th", false) => {
                    if cell_in_row > 0 {
                        out.push('|');
                    }
                    out.push(' ');
                    cell_in_row += 1;
                }
                ("td" | "th", true) => {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
                ("tr", true) => out.push('|'),
                ("pre", false) => {
                    newline(&mut out, 2);
                    out.push_str("```\n");
                    in_pre = true;
                }
                ("pre", true) => {
                    newline(&mut out, 1);
                    out.push_str("```");
                    newline(&mut out, 2);
                    in_pre = false;
                }
                ("code", _) if !in_pre => out.push('`'),
                ("strong" | "b", _) => out.push_str("**"),
                ("em" | "i", _) => out.push('*'),
                ("a", false) => {
                    link = attr(tag, "href").filter(|h| !h.starts_with('#') && !h.starts_with("javascript:"));
                    if link.is_some() {
                        out.push('[');
                    }
                }
                ("a", true) => {
                    if let Some(h) = link.take() {
                        let abs = if h.starts_with("http://") || h.starts_with("https://") {
                            h
                        } else if h.starts_with("//") {
                            format!("https:{h}")
                        } else if h.starts_with('/') {
                            format!("{base_root}{h}")
                        } else {
                            h
                        };
                        out.push_str(&format!("]({abs})"));
                    }
                }
                ("img", _) => {
                    if let Some(alt) = attr(tag, "alt").filter(|a| !a.is_empty()) {
                        out.push_str(&format!("[image: {alt}]"));
                    }
                }
                _ => {}
            }
        } else {
            let e = html[i..].find('<').map(|e| i + e).unwrap_or(b.len());
            let raw = &html[i..e];
            i = e;
            if !skip_depth.is_empty() {
                continue;
            }
            let text = decode_entities(raw);
            if in_title {
                let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    title = Some(t);
                }
                continue;
            }
            if in_pre {
                out.push_str(&text);
            } else {
                let squashed = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if squashed.is_empty() && !text.is_empty() && !out.is_empty() && !out.ends_with(['\n', ' ']) {
                    out.push(' '); // whitespace between inline tags: "<b>a</b> <b>b</b>"
                }
                if !squashed.is_empty() {
                    if text.starts_with(char::is_whitespace) && !out.ends_with(['\n', ' ', '[', '(']) {
                        out.push(' ');
                    }
                    out.push_str(&squashed);
                    if text.ends_with(char::is_whitespace) {
                        out.push(' ');
                    }
                }
            }
        }
    }
    let md: Vec<&str> = out.lines().map(|l| l.trim_end()).collect();
    let mut collapsed = String::new();
    let mut blank = 0;
    for l in md {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        collapsed.push_str(l);
        collapsed.push('\n');
    }
    (title, collapsed.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_structure_and_drops_chrome() {
        let html = r#"<html><head><title>Wrecks &amp; Reefs</title><style>p{}</style><script>alert(1)</script></head>
<body><nav><a href="/x">menu</a></nav><h1>Straits</h1><p>The <b>Cedarville</b> sank in <a href="/1965">1965</a>.</p>
<ul><li>One</li><li>Two</li></ul><pre>let x = 1;</pre><div hidden>secret</div>
<table><tr><th>Ship</th><th>Year</th></tr><tr><td>Eber Ward</td><td>1909</td></tr></table><footer>c</footer></body></html>"#;
        let (t, md) = html_to_markdown(html, "https://example.org/page");
        assert_eq!(t.as_deref(), Some("Wrecks & Reefs"));
        assert!(md.starts_with("# Straits"));
        assert!(md.contains("The **Cedarville** sank in [1965](https://example.org/1965)."));
        assert!(md.contains("- One\n- Two"));
        assert!(md.contains("```\nlet x = 1;\n```"));
        assert!(md.contains("| Ship | Year |") && md.contains("| Eber Ward | 1909 |"));
        for gone in ["menu", "alert", "secret", "p{}"] {
            assert!(!md.contains(gone), "{gone} leaked: {md}");
        }
    }

    #[test]
    fn entities_and_attrs() {
        assert_eq!(decode_entities("a &lt;b&gt; &#65;&#x42; &bogus; &"), "a <b> AB &bogus; &");
        assert_eq!(attr(r#"a href="/x" data-href='y'"#, "href").as_deref(), Some("/x"));
        assert_eq!(html_to_markdown("<p><b>Straits</b> <b>of</b> <b>Mackinac</b></p>", "").1, "**Straits** **of** **Mackinac**");
    }
}

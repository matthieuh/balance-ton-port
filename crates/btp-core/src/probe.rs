//! Title and favicon of the page a localhost server serves, for the app's
//! rows. Plain std sockets, loopback only (`127.0.0.1`, then `::1`), always
//! on the server's own port. The app's own connections are kept out of the
//! server's use by `Watcher::with_ignored_peers`.

use crate::model::ServerKey;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
pub const MAX_PAGE_BYTES: usize = 524_288;
pub const MAX_ICON_BYTES: usize = 262_144;
pub const META_TTL_MS: u64 = 600_000;
pub const FAILURE_TTL_MS: u64 = 120_000;
/// New probes started by one pass at most.
pub const MAX_PROBES_PER_PASS: usize = 2;

const MAX_REDIRECTS: usize = 2;
const MAX_TITLE_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favicon {
    pub bytes: Vec<u8>,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMeta {
    pub title: Option<String>,
    pub icon: Option<Favicon>,
}

// ---------------------------------------------------------------- parsing

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').filter(|&e| e <= 10).and_then(|e| {
            let c = match &rest[1..e] {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                n => {
                    let num = n.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
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

/// The first `<title>`, entities decoded, whitespace collapsed, at most 80
/// characters.
pub fn parse_title(html: &str) -> Option<String> {
    // ASCII lowercasing keeps byte offsets, so `lower` indexes `html`.
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    let open = loop {
        let i = from + lower[from..].find("<title")?;
        match lower.as_bytes().get(i + 6) {
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => break i,
            _ => from = i + 6,
        }
    };
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    let text = decode_entities(&html[start..end]);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_TITLE_CHARS).collect())
}

/// Attributes of a tag body (after the tag name), names lowercased.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let b = tag.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let ns = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if ns == i {
            break;
        }
        let name = tag[ns..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                i = vs;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = tag[vs..i].to_string();
                i = (i + 1).min(b.len());
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = tag[vs..i].to_string();
            }
        }
        out.push((name, decode_entities(&value)));
    }
    out
}

/// The `href` of the best `<link rel="... icon ...">`: a plain `icon` (or
/// `shortcut icon`) before an `apple-touch-icon`.
pub fn icon_href(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut other: Option<String> = None;
    let mut from = 0;
    while let Some(i) = lower[from..].find("<link") {
        let tag_start = from + i + 5;
        from = tag_start;
        if !lower[tag_start..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_whitespace())
        {
            continue;
        }
        let Some(e) = lower[tag_start..].find('>') else {
            break;
        };
        let attrs = attributes(&html[tag_start..tag_start + e]);
        from = tag_start + e;
        let rel = attrs
            .iter()
            .find(|a| a.0 == "rel")
            .map(|a| a.1.to_ascii_lowercase());
        let href = attrs
            .iter()
            .find(|a| a.0 == "href")
            .map(|a| a.1.trim().to_string());
        let (Some(rel), Some(href)) = (rel, href) else {
            continue;
        };
        if href.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = rel.split_whitespace().collect();
        if tokens.contains(&"mask-icon") || !tokens.iter().any(|t| t.contains("icon")) {
            continue;
        }
        if tokens.contains(&"icon") {
            return Some(href);
        }
        if other.is_none() {
            other = Some(href);
        }
    }
    other
}

/// The request path of an icon `href` when it points at this very server:
/// relative, root-relative, or absolute on `localhost`/`127.0.0.1` with
/// `port`. Any other host or port, and `data:` URIs (see `data_icon`),
/// give `None`.
pub fn local_icon_path(href: &str, port: u16) -> Option<String> {
    let path = local_icon_path_unchecked(href, port)?;
    // Nothing that could break out of the request line.
    (!path.chars().any(|c| c.is_whitespace() || c.is_control())).then_some(path)
}

fn local_icon_path_unchecked(href: &str, port: u16) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("data:") || href.starts_with("//") {
        return None;
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let after = &href[href.len() - rest.len()..];
        let (authority, path) = match after.find(['/', '?', '#']) {
            Some(i) => (&after[..i], &after[i..]),
            None => (after, "/"),
        };
        return authority_is_local(authority, port).then(|| absolute_path(path));
    }
    if lower.contains("://") || lower.starts_with("mailto:") {
        return None;
    }
    // A relative reference resolves against `/` (the probed page).
    Some(absolute_path(href))
}

fn absolute_path(p: &str) -> String {
    let p = p.split('#').next().unwrap_or("");
    if p.starts_with('/') {
        p.to_string()
    } else if p.starts_with('?') {
        format!("/{p}")
    } else {
        format!("/{}", p.trim_start_matches("./"))
    }
}

fn authority_is_local(authority: &str, port: u16) -> bool {
    let authority = authority.to_ascii_lowercase();
    let (host, p) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => (h, p.parse::<u16>().ok()),
        _ => (authority.as_str(), Some(80)),
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]") && p == Some(port)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// An icon inlined as a `data:` URI. Only images, at most `MAX_ICON_BYTES`.
pub fn data_icon(href: &str) -> Option<Favicon> {
    let href = href.trim();
    let rest = href
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("data:"))
        .map(|_| &href[5..])?;
    let (head, data) = rest.split_once(',')?;
    let mut parts = head.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let b64 = parts.any(|p| p.trim().eq_ignore_ascii_case("base64"));
    if !mime.starts_with("image/") {
        return None;
    }
    let bytes = if b64 {
        base64_decode(data)?
    } else {
        percent_decode(data)
    };
    if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
        return None;
    }
    Some(Favicon { bytes, mime })
}

// ------------------------------------------------------------------- HTTP

struct Response {
    status: u16,
    location: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
    /// The body was longer than the cap and was cut.
    truncated: bool,
}

fn dechunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let Some(nl) = raw[i..].windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_line = String::from_utf8_lossy(&raw[i..i + nl]);
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_hex, 16) else {
            break;
        };
        i += nl + 2;
        if size == 0 {
            break;
        }
        let Some(end) = i.checked_add(size) else {
            break;
        };
        let end = end.min(raw.len());
        out.extend_from_slice(&raw[i..end]);
        i = end.saturating_add(2);
    }
    out
}

fn parse_response(raw: &[u8], max_body: usize) -> Option<Response> {
    if !raw.starts_with(b"HTTP/1.") {
        return None;
    }
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let (mut location, mut content_type, mut chunked) = (None, None, false);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "location" => location = Some(v.to_string()),
            "content-type" => content_type = Some(v.to_string()),
            "transfer-encoding" if v.to_ascii_lowercase().contains("chunked") => chunked = true,
            _ => {}
        }
    }
    let rest = &raw[split + 4..];
    let mut body = if chunked {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    let truncated = body.len() > max_body;
    body.truncate(max_body);
    Some(Response {
        status,
        location,
        content_type,
        body,
        truncated,
    })
}

/// Whether `raw` already holds a whole response: the head and a body of
/// `Content-Length` bytes, or the last chunk. Servers that ignore
/// `Connection: close` are not waited on.
fn response_complete(raw: &[u8]) -> bool {
    let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];
    if head.contains("transfer-encoding: chunked") {
        return body.ends_with(b"0\r\n\r\n");
    }
    head.lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .is_some_and(|n| body.len() >= n)
}

/// One GET to `addr`; `None` when nothing HTTP answers within the timeout.
fn get_once(
    addr: SocketAddr,
    port: u16,
    path: &str,
    max_body: usize,
    deadline: Instant,
) -> Option<Response> {
    let left = |d: Instant| {
        d.checked_duration_since(Instant::now())
            .filter(|l| !l.is_zero())
    };
    let mut s = TcpStream::connect_timeout(&addr, left(deadline)?).ok()?;
    s.set_write_timeout(Some(left(deadline)?)).ok()?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: text/html,image/*;q=0.8,*/*;q=0.5\r\nUser-Agent: balance-ton-port\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    // Headers plus the body cap, so a huge page cannot grow without bound.
    let cap = max_body + 16_384;
    while raw.len() < cap {
        let Some(l) = left(deadline) else { break };
        s.set_read_timeout(Some(l)).ok()?;
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
        if response_complete(&raw) {
            break;
        }
    }
    parse_response(&raw, max_body)
}

fn loopbacks(port: u16) -> [SocketAddr; 2] {
    [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
    ]
}

/// A redirect target that stays on this server: a path, or an absolute URL on
/// a local host name with `port`.
fn redirect_path(location: &str, port: u16) -> Option<String> {
    local_icon_path(location, port)
}

/// GET `path` on the loopback addresses in turn, following at most two
/// redirects within the same port.
fn get(port: u16, path: &str, max_body: usize, deadline: Instant) -> Option<Response> {
    let mut path = path.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let r = loopbacks(port)
            .into_iter()
            .find_map(|a| get_once(a, port, &path, max_body, deadline))?;
        if (300..400).contains(&r.status) {
            path = redirect_path(r.location.as_deref()?, port)?;
            continue;
        }
        return Some(r);
    }
    None
}

fn guess_mime(path: &str) -> Option<&'static str> {
    let p = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = p.rsplit('.').next()?;
    Some(match ext {
        "ico" => "image/x-icon",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

fn fetch_icon(port: u16, href: Option<&str>, deadline: Instant) -> Option<Favicon> {
    if let Some(h) = href {
        if let Some(icon) = data_icon(h) {
            return Some(icon);
        }
    }
    let path = match href {
        Some(h) => local_icon_path(h, port)?,
        None => "/favicon.ico".to_string(),
    };
    let r = get(port, &path, MAX_ICON_BYTES, deadline)?;
    if r.status != 200 || r.body.is_empty() || r.truncated {
        return None;
    }
    let declared = r
        .content_type
        .as_deref()
        .map(|c| {
            c.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|c| !c.is_empty());
    // A page served for a missing icon (single-page apps) is not an image.
    let mime = match declared {
        Some(c) if c.starts_with("image/") => c,
        Some(_) => return None,
        None => guess_mime(&path)?.to_string(),
    };
    Some(Favicon {
        bytes: r.body,
        mime,
    })
}

/// Title and favicon of `GET /` on `port`, `None` when nothing HTTP answers
/// or the page has neither.
pub fn fetch_page_meta(port: u16) -> Option<PageMeta> {
    // One budget for the whole probe: page, redirects, both addresses, icons.
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let page = get(port, "/", MAX_PAGE_BYTES, deadline)?;
    if !(200..300).contains(&page.status) {
        return None;
    }
    let html = String::from_utf8_lossy(&page.body);
    let title = parse_title(&html);
    let href = icon_href(&html);
    let mut icon = fetch_icon(port, href.as_deref(), deadline);
    if icon.is_none() && href.is_some() {
        icon = fetch_icon(port, None, deadline);
    }
    if title.is_none() && icon.is_none() {
        return None;
    }
    Some(PageMeta { title, icon })
}

// ------------------------------------------------------------------ cache

#[derive(Debug)]
struct Entry {
    at_ms: u64,
    meta: Option<PageMeta>,
}

/// Probe results by (server, port): successes live `META_TTL_MS`, failures
/// (including non-HTTP ports) `FAILURE_TTL_MS`.
#[derive(Debug, Default)]
pub struct ProbeCache {
    entries: HashMap<(ServerKey, u16), Entry>,
}

impl ProbeCache {
    pub fn get(&self, key: ServerKey, port: u16) -> Option<&PageMeta> {
        self.entries.get(&(key, port))?.meta.as_ref()
    }

    fn fresh(e: &Entry, now_ms: u64) -> bool {
        let ttl = if e.meta.is_some() {
            META_TTL_MS
        } else {
            FAILURE_TTL_MS
        };
        now_ms.saturating_sub(e.at_ms) < ttl
    }

    /// The entries of `wanted` that are missing or expired, at most
    /// `MAX_PROBES_PER_PASS`: never probed first, then the oldest.
    pub fn due(&self, wanted: &[(ServerKey, u16)], now_ms: u64) -> Vec<(ServerKey, u16)> {
        let mut due: Vec<(ServerKey, u16)> = Vec::new();
        for k in wanted {
            if !due.contains(k) && self.entries.get(k).is_none_or(|e| !Self::fresh(e, now_ms)) {
                due.push(*k);
            }
        }
        due.sort_by_key(|k| self.entries.get(k).map(|e| e.at_ms));
        due.truncate(MAX_PROBES_PER_PASS);
        due
    }

    /// Records probe answers. A failed refresh keeps the previous answer.
    pub fn store(&mut self, results: Vec<((ServerKey, u16), Option<PageMeta>)>, now_ms: u64) {
        for (k, fetched) in results {
            let old = self.entries.remove(&k).and_then(|e| e.meta);
            self.entries.insert(
                k,
                Entry {
                    at_ms: now_ms,
                    meta: fetched.or(old),
                },
            );
        }
    }

    /// Forgets the entries of servers that are gone.
    pub fn retain(&mut self, wanted: &[(ServerKey, u16)]) {
        self.entries.retain(|k, _| wanted.contains(k));
    }

    /// Test helper: one whole pass with an injected fetch.
    #[cfg(test)]
    fn refresh(
        &mut self,
        wanted: &[(ServerKey, u16)],
        now_ms: u64,
        mut fetch: impl FnMut(u16) -> Option<PageMeta>,
    ) {
        self.retain(wanted);
        let due = self.due(wanted, now_ms);
        let results = due.into_iter().map(|k| (k, fetch(k.1))).collect();
        self.store(results, now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn key(pid: i32) -> ServerKey {
        ServerKey { pid, start_time: 1 }
    }

    /// Serves `routes` (path -> (content type, body)) on an ephemeral port.
    fn serve(routes: Vec<(&'static str, &'static str, Vec<u8>)>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let found = routes.iter().find(|r| r.0 == path);
                let resp = match found {
                    Some((_, ct, body)) => {
                        let mut v = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        v.extend_from_slice(body);
                        v
                    }
                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                };
                let _ = s.write_all(&resp);
            }
        });
        port
    }

    #[test]
    fn parse_title_decodes_entities() {
        assert_eq!(
            parse_title("<html><title> Revva &amp; co </title>").as_deref(),
            Some("Revva & co")
        );
        assert_eq!(
            parse_title("<TITLE>a &lt;b&gt; &quot;c&quot; it&#39;s &#65;</TITLE>").as_deref(),
            Some("a <b> \"c\" it's A")
        );
        assert_eq!(
            parse_title("<title>\n  two\n\t lines </title>").as_deref(),
            Some("two lines")
        );
    }

    #[test]
    fn title_missing_is_none() {
        assert_eq!(parse_title("<html><body>hi</body></html>"), None);
        assert_eq!(parse_title("<title>   </title>"), None);
        assert_eq!(parse_title("<titlebar>x</titlebar>"), None);
    }

    #[test]
    fn title_is_truncated_at_80_chars() {
        let html = format!("<title>{}</title>", "é".repeat(200));
        assert_eq!(parse_title(&html).unwrap().chars().count(), 80);
    }

    #[test]
    fn icon_href_prefers_icon() {
        let html = r#"<link rel="apple-touch-icon" href="/touch.png">
            <link rel='shortcut icon' href='/fav.svg'>
            <link rel="stylesheet" href="/a.css">"#;
        assert_eq!(icon_href(html).as_deref(), Some("/fav.svg"));
        let only_touch = r#"<link href=/t.png rel=apple-touch-icon>"#;
        assert_eq!(icon_href(only_touch).as_deref(), Some("/t.png"));
        assert_eq!(icon_href(r#"<link rel="stylesheet" href="/a.css">"#), None);
        assert_eq!(icon_href(r#"<link rel="mask-icon" href="/m.svg">"#), None);
    }

    #[test]
    fn local_icon_path_rejects_other_hosts() {
        assert_eq!(local_icon_path("https://cdn.example.com/f.png", 5173), None);
        assert_eq!(local_icon_path("http://cdn.example.com/f.png", 5173), None);
        assert_eq!(local_icon_path("http://localhost:9999/f.png", 5173), None);
        assert_eq!(local_icon_path("//cdn.example.com/f.png", 5173), None);
        assert_eq!(local_icon_path("/f.png", 5173).as_deref(), Some("/f.png"));
        assert_eq!(local_icon_path("f.png", 5173).as_deref(), Some("/f.png"));
        assert_eq!(
            local_icon_path("http://localhost:5173/a/f.png?v=2", 5173).as_deref(),
            Some("/a/f.png?v=2")
        );
        assert_eq!(
            local_icon_path("http://127.0.0.1:5173", 5173).as_deref(),
            Some("/")
        );
        assert_eq!(local_icon_path("data:image/png;base64,AAAA", 5173), None);
    }

    #[test]
    fn data_uri_icons_decode() {
        let i = data_icon("data:image/png;base64,aGVsbG8=").unwrap();
        assert_eq!(i.bytes, b"hello");
        assert_eq!(i.mime, "image/png");
        let s = data_icon("data:image/svg+xml,%3Csvg%2F%3E").unwrap();
        assert_eq!(s.bytes, b"<svg/>");
        assert_eq!(data_icon("data:text/html;base64,aGVsbG8="), None);
    }

    #[test]
    fn fetch_reads_title_and_icon() {
        let page =
            br#"<html><head><title>My App</title><link rel="icon" href="/logo.png"></head></html>"#;
        let port = serve(vec![
            ("/", "text/html; charset=utf-8", page.to_vec()),
            ("/logo.png", "image/png", vec![1, 2, 3]),
        ]);
        let m = fetch_page_meta(port).unwrap();
        assert_eq!(m.title.as_deref(), Some("My App"));
        let icon = m.icon.unwrap();
        assert_eq!(icon.bytes, vec![1, 2, 3]);
        assert_eq!(icon.mime, "image/png");
    }

    #[test]
    fn fetch_falls_back_to_favicon_ico() {
        let port = serve(vec![
            ("/", "text/html", b"<title>x</title>".to_vec()),
            ("/favicon.ico", "image/x-icon", vec![9, 9]),
        ]);
        let m = fetch_page_meta(port).unwrap();
        assert_eq!(m.icon.unwrap().bytes, vec![9, 9]);
    }

    #[test]
    fn spa_fallback_html_is_not_an_icon() {
        let html = b"<title>spa</title>".to_vec();
        let port = serve(vec![
            ("/", "text/html", html.clone()),
            ("/favicon.ico", "text/html", html),
        ]);
        let m = fetch_page_meta(port).unwrap();
        assert_eq!(m.title.as_deref(), Some("spa"));
        assert!(m.icon.is_none());
    }

    #[test]
    fn same_port_redirect_is_followed_other_is_not() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let resp = match path.as_str() {
                    "/" => "HTTP/1.1 302 Found\r\nLocation: /home\r\nContent-Length: 0\r\n\r\n"
                        .to_string(),
                    "/home" => {
                        let b = "<title>home</title>";
                        format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{b}", b.len())
                    }
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string(),
                };
                let _ = s.write_all(resp.as_bytes());
            }
        });
        assert_eq!(
            fetch_page_meta(port).unwrap().title.as_deref(),
            Some("home")
        );

        let away = serve_raw(
            b"HTTP/1.1 302 Found\r\nLocation: http://example.com/\r\nContent-Length: 0\r\n\r\n",
        );
        assert_eq!(fetch_page_meta(away), None);
        let other_port = serve_raw(
            b"HTTP/1.1 302 Found\r\nLocation: http://localhost:1/\r\nContent-Length: 0\r\n\r\n",
        );
        assert_eq!(fetch_page_meta(other_port), None);
    }

    #[test]
    fn chunked_pages_are_decoded() {
        let body = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/html\r\n\r\n7\r\n<title>\r\n3\r\nabc\r\n8\r\n</title>\r\n0\r\n\r\n";
        let port = serve_raw(body);
        assert_eq!(fetch_page_meta(port).unwrap().title.as_deref(), Some("abc"));
    }

    fn serve_raw(resp: &'static [u8]) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(resp);
            }
        });
        port
    }

    #[test]
    fn non_http_response_is_none() {
        let port = serve_raw(b"SSH-2.0-x\r\n");
        assert_eq!(fetch_page_meta(port), None);
    }

    #[test]
    fn slow_server_times_out() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hold = thread::spawn(move || {
            let held: Vec<_> = l.incoming().take(2).collect();
            thread::sleep(Duration::from_secs(3));
            drop(held);
        });
        let t = Instant::now();
        assert_eq!(fetch_page_meta(port), None);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        drop(hold);
    }

    #[test]
    fn nothing_listening_is_none() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert_eq!(fetch_page_meta(port), None);
    }

    #[test]
    fn huge_chunk_size_does_not_panic() {
        let port = serve_raw(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nabc\r\n0\r\n\r\n",
        );
        assert_eq!(fetch_page_meta(port), None);
        assert!(dechunk(b"ffffffffffffffff\r\nabc").len() <= 3);
    }

    #[test]
    fn redirect_chain_to_a_hanging_path_is_bounded() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            let mut held = Vec::new();
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                match req.split_whitespace().nth(1) {
                    Some("/") => {
                        let _ = s.write_all(
                            b"HTTP/1.1 302 Found\r\nLocation: /a\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                    Some("/a") => {
                        let _ = s.write_all(
                            b"HTTP/1.1 302 Found\r\nLocation: /b\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                    _ => held.push(s),
                }
            }
        });
        let t = Instant::now();
        assert_eq!(fetch_page_meta(port), None);
        assert!(
            t.elapsed() < Duration::from_millis(1500),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn keep_alive_server_with_content_length_returns_promptly() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            let mut held = Vec::new();
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let b = "<title>ka</title>";
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{b}",
                        b.len()
                    )
                    .as_bytes(),
                );
                held.push(s); // never closed
            }
        });
        let t = Instant::now();
        assert_eq!(fetch_page_meta(port).unwrap().title.as_deref(), Some("ka"));
        assert!(
            t.elapsed() < Duration::from_millis(600),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn oversized_icon_gives_no_icon() {
        let port = serve(vec![
            ("/", "text/html", b"<title>big</title>".to_vec()),
            ("/favicon.ico", "image/x-icon", vec![7; MAX_ICON_BYTES + 10]),
        ]);
        let m = fetch_page_meta(port).unwrap();
        assert_eq!(m.title.as_deref(), Some("big"));
        assert!(m.icon.is_none());
    }

    #[test]
    fn paths_with_whitespace_or_control_chars_are_rejected() {
        assert_eq!(local_icon_path("/a b.png", 1), None);
        assert_eq!(local_icon_path("/a\r\nHost: x", 1), None);
        assert_eq!(local_icon_path("/a\u{7f}", 1), None);
    }

    #[test]
    fn due_dedups_non_adjacent_duplicates() {
        let c = ProbeCache::default();
        let w = [(key(1), 1), (key(2), 2), (key(1), 1), (key(3), 3)];
        assert_eq!(c.due(&w, 0), vec![(key(1), 1), (key(2), 2)]);
    }

    fn meta(t: &str) -> Option<PageMeta> {
        Some(PageMeta {
            title: Some(t.into()),
            icon: None,
        })
    }

    #[test]
    fn failure_is_cached_two_minutes() {
        let mut c = ProbeCache::default();
        let wanted = [(key(1), 3000)];
        let mut calls = 0;
        c.refresh(&wanted, 0, |_| {
            calls += 1;
            None
        });
        c.refresh(&wanted, FAILURE_TTL_MS - 1, |_| {
            calls += 1;
            None
        });
        assert_eq!(calls, 1);
        c.refresh(&wanted, FAILURE_TTL_MS, |_| {
            calls += 1;
            None
        });
        assert_eq!(calls, 2);
    }

    #[test]
    fn success_is_cached_ten_minutes_and_kept_on_failed_refresh() {
        let mut c = ProbeCache::default();
        let wanted = [(key(1), 3000)];
        c.refresh(&wanted, 0, |_| meta("a"));
        let mut calls = 0;
        c.refresh(&wanted, META_TTL_MS - 1, |_| {
            calls += 1;
            None
        });
        assert_eq!(calls, 0);
        c.refresh(&wanted, META_TTL_MS, |_| {
            calls += 1;
            None
        });
        assert_eq!(calls, 1);
        assert_eq!(c.get(key(1), 3000).unwrap().title.as_deref(), Some("a"));
    }

    #[test]
    fn at_most_two_new_probes_per_pass_and_gone_servers_are_forgotten() {
        let mut c = ProbeCache::default();
        let wanted = [(key(1), 1), (key(2), 2), (key(3), 3)];
        let mut probed = Vec::new();
        c.refresh(&wanted, 0, |p| {
            probed.push(p);
            meta("x")
        });
        assert_eq!(probed, vec![1, 2]);
        c.refresh(&wanted, 1, |p| {
            probed.push(p);
            meta("x")
        });
        assert_eq!(probed, vec![1, 2, 3]);
        c.refresh(&wanted[..1], 2, |_| meta("x"));
        assert!(c.get(key(3), 3).is_none());
        assert!(c.get(key(1), 1).is_some());
    }
}

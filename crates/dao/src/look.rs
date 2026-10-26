//! look — render an HTML page in headless Chrome and report style-neutral visual defects.
//!
//! Role: the machine layers (L1 rules, L2 metrics) of judging a visual skill's output, so any
//! hosted skill that produces pages can be measured without bringing its own checker. It never
//! scores taste: direction-specific rubrics and blind preference stay outside this module.
//! Depends on: a Chrome/Chromium binary (DAO_BROWSER, else PATH, else the macOS app path) and
//! look.js (the in-page measurement). Called by: `main` (`dao look`).
//! Invariants: read-only on the page (it renders a temporary copy whose <base> points back, inside
//! an iframe of exactly the requested width);
//! network is blocked (unresolvable names plus a dead proxy for every http(s) request); each
//! render has a deadline; a page that cannot be rendered, renders at another width, or shows no
//! measurable text is an environment error (exit 2), never a clean result. The measurement runs
//! inside the page, so it is only valid for cooperative pages: hostile page script can alter it.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MEASURE_JS: &str = include_str!("look.js");
pub const DEFAULT_WIDTHS: [u32; 2] = [1280, 375];
const RENDER_TIMEOUT_S: u64 = 60;

pub fn find_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DAO_BROWSER") {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        for dir in std::env::split_paths(&path) {
            if dir.join(name).is_file() {
                return Some(dir.join(name));
            }
        }
    }
    let mac = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    mac.is_file().then_some(mac)
}

/// `page` rendered at each width; Ok(report) with status "clean" or "defects".
pub fn look(browser: &Path, page: &Path, widths: &[u32]) -> Result<Value, String> {
    let page = page
        .canonicalize()
        .map_err(|e| format!("{}: {e}", page.display()))?;
    let html = fs::read_to_string(&page).map_err(|e| format!("{}: {e}", page.display()))?;
    let dir = page.parent().ok_or("page has no parent directory")?;
    let nonce = nonce();
    // a fresh directory (create_dir fails if it exists) and an attribute name the page cannot know
    let work = std::env::temp_dir().join(format!("dao-look-{nonce}"));
    fs::create_dir(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let attr = format!("data-dao-look-{nonce}");
    let result = (|| {
        let inner = instrument(&html, dir, &attr);
        let mut views = Vec::new();
        for &w in widths {
            let copy = work.join(format!("page-{w}.html"));
            fs::write(&copy, frame(&inner, w)).map_err(|e| e.to_string())?;
            views.push(render(browser, &copy, &work, w, &attr)?);
        }
        let defects = views
            .iter()
            .any(|v| v["findings"].as_array().is_some_and(|f| !f.is_empty()));
        Ok(json!({
            "path": page.display().to_string(),
            "views": views,
            "status": if defects { "defects" } else { "clean" },
        }))
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

fn nonce() -> String {
    static RUN: AtomicUsize = AtomicUsize::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seed = format!(
        "{t}-{}-{}",
        std::process::id(),
        RUN.fetch_add(1, Ordering::Relaxed)
    );
    Sha256::digest(seed.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `file://` URL for a path, every byte outside unreserved characters and `/` percent-encoded.
fn file_url(p: &Path) -> String {
    let mut out = String::from("file://");
    for &b in p.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The page with a <base> back to its own folder (right after the doctype, so the parser puts it
/// in the head without changing the rendering mode) and the measurement before </body>.
fn instrument(html: &str, dir: &Path, attr: &str) -> String {
    // resource load errors do not bubble; a capturing listener installed first sees all of them
    let base = format!(
        "<base href=\"{}/\"><script>window.__daoMissing=[];addEventListener(\"error\",\
         function(e){{var t=e.target;if(t&&t.getAttribute&&(t.src||t.href))__daoMissing.push(t);}},true);\
         </script>",
        file_url(dir)
    );
    let script = format!(
        "<script>{}</script>",
        MEASURE_JS.replace("__DAO_ATTR__", attr)
    );
    let lower = html.to_ascii_lowercase();
    let mut out = html.to_string();
    match lower.rfind("</body>") {
        Some(i) => out.insert_str(i, &script),
        None => out.push_str(&script),
    }
    let start = lower.len()
        - lower
            .trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n'])
            .len();
    let at = if lower[start..].starts_with("<!doctype") {
        lower[start..].find('>').map_or(0, |i| start + i + 1)
    } else {
        0
    };
    out.insert_str(at, &base);
    out
}

/// An outer page holding the instrumented page in a srcdoc iframe exactly `width` px wide:
/// headless Chrome will not size a window below about 500 px, an iframe has no such floor, and
/// a srcdoc frame shares the outer document's origin so the measurement can be written back.
fn frame(inner: &str, width: u32) -> String {
    let doc = inner.replace('&', "&amp;").replace('"', "&quot;");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body style=\"margin:0\">\
         <iframe style=\"display:block;border:0;width:{width}px;height:900px\" srcdoc=\"{doc}\">\
         </iframe></body></html>"
    )
}

fn render(
    browser: &Path,
    copy: &Path,
    work: &Path,
    width: u32,
    attr: &str,
) -> Result<Value, String> {
    let dump = work.join(format!("dom-{width}.html"));
    let errlog = work.join(format!("err-{width}.txt"));
    let mut cmd = Command::new(browser);
    cmd.args([
        "--headless=new",
        "--disable-gpu",
        "--disable-extensions",
        "--no-first-run",
        "--no-default-browser-check",
        "--hide-scrollbars",
        // the page is data: names resolve nowhere, and every http(s) request, loopback included,
        // goes to a proxy that is not there; file:// is unaffected
        "--host-resolver-rules=MAP * ~NOTFOUND",
        "--proxy-server=http://127.0.0.1:9",
        "--proxy-bypass-list=<-loopback>",
        "--virtual-time-budget=5000",
    ])
    .arg(format!(
        "--user-data-dir={}",
        work.join("profile").display()
    ))
    .arg(format!("--window-size={},1000", width.max(600) + 40));
    // for CI hosts where Chrome's sandbox cannot start (e.g. no user namespaces); never default
    if std::env::var_os("DAO_LOOK_NO_SANDBOX").is_some() {
        cmd.arg("--no-sandbox");
    }
    let mut child = cmd
        .arg("--dump-dom")
        .arg(file_url(copy))
        .stdout(File::create(&dump).map_err(|e| e.to_string())?)
        .stderr(File::create(&errlog).map_err(|e| e.to_string())?)
        .spawn()
        .map_err(|e| format!("{}: {e}", browser.display()))?;
    let deadline = Instant::now() + Duration::from_secs(RENDER_TIMEOUT_S);
    // ponytail: kills the main browser process only; its helpers exit with it on the CI hosts used
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "render at {width}px timed out after {RENDER_TIMEOUT_S}s"
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let fail = |what: &str| {
        let err = fs::read_to_string(&errlog).unwrap_or_default();
        let tail: String = err
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!(
            "render at {width}px: {what} (browser {status}): {}",
            tail.trim()
        )
    };
    let dom = fs::read_to_string(&dump).unwrap_or_default();
    let encoded = html_attr(&dom, attr).ok_or_else(|| fail("no measurement"))?;
    let raw = percent_decode(&encoded).ok_or_else(|| fail("measurement is not percent-encoded"))?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| fail(&format!("measurement: {e}")))?;
    if v["width"].as_u64() != Some(u64::from(width)) {
        return Err(fail(&format!(
            "viewport was {} px, not {width}",
            v["width"]
        )));
    }
    if v["metrics"]["text_elements"].as_u64().unwrap_or(0) == 0 {
        return Err(fail("no visible text was measured"));
    }
    Ok(v)
}

/// The value of `attr` on the <html> start tag of a dumped DOM, comments ignored.
fn html_attr(dom: &str, attr: &str) -> Option<String> {
    let mut rest = dom;
    let mut text = String::new();
    while let Some(i) = rest.find("<!--") {
        text.push_str(&rest[..i]);
        rest = rest[i..].find("-->").map_or("", |j| &rest[i + j + 3..]);
    }
    text.push_str(rest);
    let lower = text.to_ascii_lowercase();
    let start = lower.find("<html")?;
    let end = start + lower[start..].find('>')?;
    let key = format!(" {attr}=\"");
    let tag = &text[start..end];
    let v = &tag[tag.find(&key)? + key.len()..];
    Some(v[..v.find('"')?].to_string())
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_puts_base_after_doctype_and_script_before_body_end() {
        let out = instrument(
            "<!DOCTYPE html><html><head lang=x></head><body><p>x</p></BODY></html>",
            Path::new("/a b/#?%&"),
            "data-dao-look-1",
        );
        assert!(
            out.starts_with("<!DOCTYPE html><base href=\"file:///a%20b/%23%3F%25%26/\">"),
            "{out}"
        );
        assert!(out.find("<script>").unwrap() < out.find("</BODY>").unwrap());
        assert!(out.contains("\"data-dao-look-1\""));
    }

    #[test]
    fn result_is_read_from_the_html_tag_only() {
        let dom = "<!-- <html data-dao-look-1=\"%7B%7D\"> --><html lang=\"en\" data-dao-look-1=\"%5B1%5D\"><body data-dao-look-1=\"x\"></body></html>";
        assert_eq!(html_attr(dom, "data-dao-look-1").unwrap(), "%5B1%5D");
        assert!(html_attr("<html><body data-dao-look-1=\"x\">", "data-dao-look-1").is_none());
    }

    #[test]
    fn percent_decode_round_trips_utf8() {
        assert_eq!(
            percent_decode("%7B%22a%22%3A%22%E4%B8%AD%22%7D").unwrap(),
            "{\"a\":\"中\"}"
        );
        assert!(percent_decode("%Z1").is_none());
    }
}

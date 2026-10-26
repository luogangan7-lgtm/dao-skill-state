//! look — render an HTML page in headless Chrome and report style-neutral visual defects.
//!
//! Role: the machine layers (L1 rules, L2 metrics) of judging a visual skill's output, so any
//! hosted skill that produces pages can be measured without bringing its own checker. It never
//! scores taste: direction-specific rubrics and blind preference stay outside this module.
//! Depends on: a Chrome/Chromium binary (DAO_BROWSER, else PATH, else the macOS app path) and
//! look.js (the in-page measurement). Called by: `main` (`dao look`).
//! Invariants: read-only on the page (it renders a temporary copy whose <base> points back);
//! network is blocked by host-resolver rules; each render has a deadline; a page that cannot be
//! rendered or measured is an environment error (exit 2), never a clean result.

use serde_json::{json, Value};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
    static RUN: AtomicUsize = AtomicUsize::new(0);
    let work = std::env::temp_dir().join(format!(
        "dao-look-{}-{}",
        std::process::id(),
        RUN.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let copy = work.join("page.html");
        fs::write(&copy, instrument(&html, dir)).map_err(|e| e.to_string())?;
        let mut views = Vec::new();
        for &w in widths {
            views.push(render(browser, &copy, &work, w)?);
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

/// The page with a <base> back to its own folder (so relative assets resolve) and the measurement.
fn instrument(html: &str, dir: &Path) -> String {
    let base = format!(
        "<base href=\"file://{}/\">",
        dir.display().to_string().replace('"', "%22")
    );
    let script = format!("<script>{MEASURE_JS}</script>");
    let lower = html.to_ascii_lowercase();
    let mut out = html.to_string();
    match lower.rfind("</body>") {
        Some(i) => out.insert_str(i, &script),
        None => out.push_str(&script),
    }
    match lower.find("<head>") {
        Some(i) => out.insert_str(i + "<head>".len(), &base),
        None => out.insert_str(0, &base),
    }
    out
}

fn render(browser: &Path, copy: &Path, work: &Path, width: u32) -> Result<Value, String> {
    let dump = work.join(format!("dom-{width}.html"));
    let mut cmd = Command::new(browser);
    cmd.args([
        "--headless=new",
        "--disable-gpu",
        "--disable-extensions",
        "--no-first-run",
        "--no-default-browser-check",
        "--hide-scrollbars",
        // the page is data: no network, local files only
        "--host-resolver-rules=MAP * ~NOTFOUND",
        "--virtual-time-budget=5000",
    ])
    .arg(format!(
        "--user-data-dir={}",
        work.join("profile").display()
    ))
    .arg(format!("--window-size={width},900"));
    if std::env::var_os("DAO_LOOK_NO_SANDBOX").is_some() {
        cmd.arg("--no-sandbox");
    }
    let mut child = cmd
        .arg("--dump-dom")
        .arg(format!("file://{}", copy.display()))
        .stdout(File::create(&dump).map_err(|e| e.to_string())?)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", browser.display()))?;
    let deadline = Instant::now() + Duration::from_secs(RENDER_TIMEOUT_S);
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
    let dom = fs::read_to_string(&dump).unwrap_or_default();
    let encoded = dom
        .split("data-dao-look=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .ok_or_else(|| format!("render at {width}px produced no measurement (browser {status})"))?;
    let raw = percent_decode(&encoded.replace("&amp;", "&"))
        .ok_or("measurement is not valid percent-encoding")?;
    serde_json::from_str(&raw).map_err(|e| format!("measurement: {e}"))
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
    fn instrument_adds_base_and_script() {
        let out = instrument(
            "<html><HEAD></HEAD><body><p>x</p></BODY></html>",
            Path::new("/a b"),
        );
        assert!(out.contains("<HEAD><base href=\"file:///a b/\">"), "{out}");
        assert!(out.find("<script>").unwrap() < out.find("</BODY>").unwrap());
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

//! Calibration gate for `dao look`: known-good pages must come back clean, and each known-bad
//! page must show exactly the defect it was built with, checked separately at desktop (1280) and
//! phone (375) width. A second test proves the page cannot reach the network. A browser is
//! required on CI; elsewhere the tests are skipped with a note when none is found.

use std::collections::BTreeSet;
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

type Kinds = &'static [&'static str];
/// (page, findings at 1280, findings at 375)
const CASES: &[(&str, Kinds, Kinds)] = &[
    ("good-article", &[], &[]),
    ("good-table", &[], &[]),
    ("good-landing", &[], &[]),
    ("good-dark", &[], &[]),
    ("good-wrapped", &[], &[]),
    ("bad-contrast", &["contrast"], &["contrast"]),
    ("bad-overflow", &[], &["h-scroll"]),
    (
        "bad-sprawl",
        &["font-families", "type-scale"],
        &["font-families", "type-scale"],
    ),
    ("bad-repetition", &["repetition"], &["repetition"]),
    ("bad-wrapped-repetition", &["repetition"], &["repetition"]),
    ("bad-alt", &["img-alt"], &["img-alt"]),
    ("bad-clipped", &["clipped"], &["clipped"]),
    ("bad-line-length", &["line-length"], &[]),
    ("injected-target", &["target-size"], &["target-size"]),
    ("bad-target-adjacent", &["target-size"], &["target-size"]),
];

fn browser_available() -> bool {
    let found = std::env::var_os("DAO_BROWSER").is_some()
        || [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ]
        .iter()
        .any(|n| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .any(|d| d.join(n).is_file())
        })
        || Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome").is_file();
    if !found {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI needs a browser for the look gate"
        );
        eprintln!("skipped: no Chrome/Chromium found (set DAO_BROWSER)");
    }
    found
}

fn look(page: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dao"))
        .arg("look")
        .arg(page)
        .output()
        .unwrap()
}

fn kinds(view: &serde_json::Value) -> BTreeSet<String> {
    view["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["kind"].as_str().map(String::from))
        .collect()
}

#[test]
fn calibration_pages_come_back_as_built() {
    if !browser_available() {
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/look");
    let mut wrong = Vec::new();
    for (name, desktop, phone) in CASES {
        let out = look(&dir.join(format!("{name}.html")));
        let Ok(report) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
            wrong.push(format!(
                "{name}: exit {:?}, no report: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        };
        let views = report["views"].as_array().cloned().unwrap_or_default();
        let set = |k: Kinds| k.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let want = [(1280, set(desktop)), (375, set(phone))];
        let code = if desktop.is_empty() && phone.is_empty() {
            0
        } else {
            1
        };
        let ok = views.len() == 2
            && out.status.code() == Some(code)
            && want
                .iter()
                .zip(&views)
                .all(|((w, k), v)| v["width"] == *w && kinds(v) == *k);
        if !ok {
            wrong.push(format!(
                "{name}: want {want:?}, exit {code}; got exit {:?}: {report}",
                out.status.code()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "calibration mismatches:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn page_cannot_reach_the_network() {
    if !browser_available() {
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = std::env::temp_dir().join(format!("dao-look-net-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join("net.html");
    std::fs::write(
        &page,
        format!(
            "<!doctype html><html><head><link rel=\"stylesheet\" href=\"http://127.0.0.1:{port}/probe.css\">\
             <link rel=\"stylesheet\" href=\"https://example.com/probe.css\"></head>\
             <body><p>Network probe.</p></body></html>"
        ),
    )
    .unwrap();
    let out = look(&page);
    let _ = std::fs::remove_dir_all(&dir);
    std::thread::sleep(Duration::from_millis(200));
    let mut reached = Vec::new();
    while let Ok((mut s, _)) = listener.accept() {
        let mut buf = [0u8; 256];
        let _ = s.read(&mut buf);
        reached.push(String::from_utf8_lossy(&buf).into_owned());
    }
    assert!(
        reached.is_empty(),
        "the page reached a local server: {reached:?}"
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("no report: {}", String::from_utf8_lossy(&out.stderr)));
    let missing = report["views"][0]["counts"]["missing-resource"].as_u64();
    assert_eq!(
        missing,
        Some(2),
        "blocked stylesheets must be reported: {report}"
    );
    assert_eq!(out.status.code(), Some(1));
}

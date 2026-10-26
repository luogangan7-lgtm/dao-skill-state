//! Calibration gate for `dao look`: known-good pages must come back clean at desktop and phone
//! widths, and each known-bad page must show exactly the defect it was built with. A browser is
//! required on CI; elsewhere the test is skipped with a note when none is found.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const CASES: &[(&str, &[&str])] = &[
    ("good-article", &[]),
    ("good-table", &[]),
    ("good-landing", &[]),
    ("bad-contrast", &["contrast"]),
    ("bad-overflow", &["h-scroll"]),
    ("bad-sprawl", &["font-families", "type-scale"]),
    ("bad-repetition", &["repetition"]),
    ("injected-target", &["target-size"]),
];

fn browser_available() -> bool {
    std::env::var_os("DAO_BROWSER").is_some()
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
        || Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome").is_file()
}

#[test]
fn calibration_pages_come_back_as_built() {
    if !browser_available() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI needs a browser for the look gate"
        );
        eprintln!("skipped: no Chrome/Chromium found (set DAO_BROWSER)");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/look");
    let mut wrong = Vec::new();
    for (name, want) in CASES {
        let out = Command::new(env!("CARGO_BIN_EXE_dao"))
            .arg("look")
            .arg(dir.join(format!("{name}.html")))
            .output()
            .unwrap();
        let report: serde_json::Value = match serde_json::from_slice(&out.stdout) {
            Ok(v) => v,
            Err(_) => {
                wrong.push(format!(
                    "{name}: exit {:?}, no report: {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
                continue;
            }
        };
        let got: BTreeSet<String> = report["views"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|v| v["findings"].as_array().cloned().unwrap_or_default())
            .filter_map(|f| f["kind"].as_str().map(String::from))
            .collect();
        let want: BTreeSet<String> = want.iter().map(|s| s.to_string()).collect();
        let code = if want.is_empty() { 0 } else { 1 };
        if got != want || out.status.code() != Some(code) {
            wrong.push(format!(
                "{name}: want {want:?}, got {got:?}, exit {:?}: {report}",
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

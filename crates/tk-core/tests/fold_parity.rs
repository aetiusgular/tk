//! The Rust fold equals the TypeScript fold on every code point whose fold
//! holds ASCII (contracts/fixtures/policy/fold.json, generated from
//! packages/tk-claude-plugin/src/core/fold.ts by gen/gen-fold-tables.mjs).
//! Integration w1: one fold for all three matchers.

use tk_core::policy::{fold_text, FOLD_UNICODE_VERSION};

#[test]
fn rust_fold_equals_the_typescript_fold() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/fixtures/policy/fold.json"
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        doc["unicode"], FOLD_UNICODE_VERSION,
        "tables and fixture from one Unicode version"
    );
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 3000, "{} cases", cases.len());
    let mut bad = Vec::new();
    for c in cases {
        let cp = u32::from_str_radix(c[0].as_str().unwrap(), 16).unwrap();
        let ch = char::from_u32(cp).unwrap();
        let want = c[1].as_str().unwrap();
        let got = fold_text(&ch.to_string());
        if got != want {
            bad.push(format!("U+{cp:04X}: want {want:?}, got {got:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} differ, first: {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
}

//! Shared loadsheet fixtures (real messages, names removed): every Airframes
//! loadsheet parser (decoder, xng, event digger) must produce exactly
//! `expected`, with `null` meaning "not a loadsheet".

use serde_json::Value;
use xng_acars::{decode, loadsheet, AcarsApp};

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/loadsheet-fixtures.json")).unwrap()
}

/// Compare JSON numerically, so 33 (JS) equals 33.0 (f64 in Rust).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

#[test]
fn parser_matches_shared_fixtures() {
    let mut failures = Vec::new();
    for f in fixtures() {
        let got = serde_json::to_value(loadsheet::parse(f["text"].as_str().unwrap())).unwrap();
        if !same(&got, &f["expected"]) {
            failures.push(format!(
                "{} ({}):\n  expected {}\n  got      {}",
                f["id"], f["description"], f["expected"], got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} fixture(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn decode_emits_loadsheet_app_on_any_label() {
    for f in fixtures() {
        let label = f["label"].as_str().unwrap();
        let d = decode(label, f["text"].as_str().unwrap(), false);
        let is_ls = matches!(d.app, Some(AcarsApp::Loadsheet(_)));
        assert_eq!(
            is_ls,
            !f["expected"].is_null(),
            "{} label {label}: {:?}",
            f["id"],
            d.app
        );
        if is_ls {
            let v = serde_json::to_value(&d.app).unwrap();
            assert_eq!(v["app"], "loadsheet");
            assert_eq!(v["kind"], f["expected"]["kind"]);
        }
    }
}

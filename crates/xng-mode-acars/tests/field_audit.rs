use num_complex::Complex;
use xng_mode_acars::modulate::{burst_iq, FrameSpec};
use xng_mode_acars::AcarsChannelDecoder;
use xng_types::{MessageBody, Provenance};

#[test]
fn audit_qq_fields_in_json() {
    let spec = FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "QQ",
        block_id: '4',
        msg_num: Some("M01A"),
        flight: Some("XG0042"),
        text: "KEWRKSWF20041942",
        etb: false,
    };
    let mut iq = vec![Complex::new(0.0, 0.0); 500];
    iq.extend(burst_iq(&spec, 24_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 500]);

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    
    let source = Provenance {
        station: xng_types::StationIdentity::new("XX-TEST-ACARS"),
        app: xng_types::AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = xng_mode_acars::to_message(&frames[0], 131_550_000, -20.0, -55.0, source);
    let MessageBody::Acars(core) = &msg.body else { panic!("not acars") };
    
    println!("\n=== QQ Message Fields ===");
    println!("sublabel: {:?}", core.sublabel);
    println!("mfi: {:?}", core.mfi);
    println!("reassembled: {}", core.reassembled);
    
    if let Some(app_json) = core.app.as_ref() {
        println!("app JSON keys: {:?}", app_json.as_object().map(|o| o.keys().collect::<Vec<_>>()));
        println!("Full JSON:\n{}", serde_json::to_string_pretty(app_json).unwrap());
    }
    
    // Verify expected fields are present
    if let Some(app_json) = core.app.as_ref() {
        assert_eq!(app_json["depa"], "KEWR", "depa should be present");
        assert_eq!(app_json["dsta"], "KSWF", "dsta should be present");
        assert_eq!(app_json["wloff"], "2004", "wloff should be present");
        assert!(app_json.get("assstat").is_none(), "assstat currently NOT surfaced");
    }
}

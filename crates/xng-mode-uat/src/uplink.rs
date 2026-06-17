//! UAT uplink ground message (432-byte MDB) and FIS-B APDU framing —
//! DO-282B §2.2.4.6 / DO-358. Field offsets, the information-frame header,
//! the FIS-B APDU header (product id, time options, segmentation flags),
//! and the DLAC text products follow dump978's `legacy/uat_decode.c`
//! (`uat_decode_uplink_mdb` / `uat_decode_info_frame`).

use crate::dlac::{self, TextReport};
use serde::Serialize;

/// Maximum information frames dump978 will parse from one uplink MDB.
const MAX_INFO_FRAMES: usize = 256;
/// Application-data length in a corrected uplink MDB (432 − 8-byte header).
const APP_DATA_LEN: usize = 424;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UplinkSite {
    pub lat: f64,
    pub lon: f64,
    /// dump978 decodes lat/lon even when this flag is clear.
    pub position_valid: bool,
}

/// Product-reference time carried by a FIS-B APDU.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct ProductTime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<u32>,
    pub hours: u32,
    pub minutes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u32>,
}

/// A FIS-B Application Protocol Data Unit (product payload + framing).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FisbProduct {
    pub product_id: u32,
    pub product_name: String,
    /// A flag — "Application Method/AID Present".
    pub a_flag: bool,
    /// G flag — Geometric overlay options present.
    pub g_flag: bool,
    /// P flag — Position present (raster/vector products).
    pub p_flag: bool,
    /// S flag — segmentation in use (multi-part APDU).
    pub s_flag: bool,
    pub time: ProductTime,
    /// Raw APDU payload (product data, after the FIS-B header).
    #[serde(skip)]
    pub data: Vec<u8>,
    /// Decoded text reports, present for DLAC-text products.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<SerReport>,
}

/// Serializable form of a [`TextReport`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SerReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    pub text: String,
}

impl From<TextReport> for SerReport {
    fn from(t: TextReport) -> Self {
        SerReport { report_type: t.report_type, location: t.location, time: t.time, text: t.text }
    }
}

/// One information frame from the uplink MDB application data.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InfoFrame {
    pub length: usize,
    /// Frame type: 0 = FIS-B APDU, 15 = TIS-B/ADS-R Service Status.
    pub frame_type: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fisb: Option<FisbProduct>,
    /// Raw frame payload when not parsed as FIS-B.
    #[serde(skip)]
    pub data: Vec<u8>,
}

/// A decoded UAT uplink ground message.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UatUplink {
    pub site: UplinkSite,
    pub utc_coupled: bool,
    pub app_data_valid: bool,
    pub slot_id: u32,
    pub tisb_site_id: u32,
    pub info_frames: Vec<InfoFrame>,
}

impl UatUplink {
    /// Decode a 432-byte corrected uplink MDB (parity already stripped).
    pub fn decode(mdb: &[u8]) -> Result<UatUplink, &'static str> {
        if mdb.len() != 432 {
            return Err("uplink MDB must be 432 bytes");
        }
        // Site position (decoded regardless of position_valid, per dump978).
        let raw_lat = ((mdb[0] as u32) << 15) | ((mdb[1] as u32) << 7) | ((mdb[2] as u32) >> 1);
        let raw_lon = (((mdb[2] as u32) & 0x01) << 23)
            | ((mdb[3] as u32) << 15)
            | ((mdb[4] as u32) << 7)
            | ((mdb[5] as u32) >> 1);
        let mut lat = raw_lat as f64 * 360.0 / 16_777_216.0;
        if lat > 90.0 {
            lat -= 180.0;
        }
        let mut lon = raw_lon as f64 * 360.0 / 16_777_216.0;
        if lon > 180.0 {
            lon -= 360.0;
        }
        let position_valid = (mdb[5] & 0x01) != 0;
        let utc_coupled = (mdb[6] & 0x80) != 0;
        let app_data_valid = (mdb[6] & 0x20) != 0;
        let slot_id = (mdb[6] & 0x1f) as u32;
        let tisb_site_id = (mdb[7] >> 4) as u32;

        let mut info_frames = Vec::new();
        if app_data_valid {
            let app = &mdb[8..8 + APP_DATA_LEN];
            let mut pos = 0usize;
            while info_frames.len() < MAX_INFO_FRAMES && pos + 2 <= app.len() {
                let length =
                    (((app[pos] as usize) << 1) | ((app[pos + 1] as usize) >> 7)) & 0x1ff;
                let frame_type = (app[pos + 1] & 0x0f) as u32;
                if pos + length + 2 > app.len() {
                    break; // overrun
                }
                if length == 0 && frame_type == 0 {
                    break; // no more frames
                }
                let data = app[pos + 2..pos + 2 + length].to_vec();
                let fisb = parse_fisb(frame_type, &data);
                info_frames.push(InfoFrame { length, frame_type, fisb, data });
                pos += length + 2;
            }
        }

        Ok(UatUplink {
            site: UplinkSite { lat, lon, position_valid },
            utc_coupled,
            app_data_valid,
            slot_id,
            tisb_site_id,
            info_frames,
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("UatUplink serializes")
    }
}

/// Parse a FIS-B APDU out of an info frame's payload (type 0, ≥4 bytes).
fn parse_fisb(frame_type: u32, data: &[u8]) -> Option<FisbProduct> {
    if frame_type != 0 || data.len() < 4 {
        return None;
    }
    let t_opt = ((data[1] & 0x01) << 1) | (data[2] >> 7);
    let mut time = ProductTime::default();
    let payload_start: usize;
    match t_opt {
        0 => {
            time.hours = ((data[2] & 0x7c) >> 2) as u32;
            time.minutes = (((data[2] & 0x03) << 4) | (data[3] >> 4)) as u32;
            payload_start = 4;
        }
        1 => {
            if data.len() < 5 {
                return None;
            }
            time.hours = ((data[2] & 0x7c) >> 2) as u32;
            time.minutes = (((data[2] & 0x03) << 4) | (data[3] >> 4)) as u32;
            time.seconds = Some((((data[3] & 0x0f) << 2) | (data[4] >> 6)) as u32);
            payload_start = 5;
        }
        2 => {
            if data.len() < 5 {
                return None;
            }
            time.month = Some(((data[2] & 0x78) >> 3) as u32);
            time.day = Some((((data[2] & 0x07) << 2) | (data[3] >> 6)) as u32);
            time.hours = ((data[3] & 0x3e) >> 1) as u32;
            time.minutes = (((data[3] & 0x01) << 5) | (data[4] >> 3)) as u32;
            payload_start = 5;
        }
        3 => {
            if data.len() < 6 {
                return None;
            }
            time.month = Some(((data[2] & 0x78) >> 3) as u32);
            time.day = Some((((data[2] & 0x07) << 2) | (data[3] >> 6)) as u32);
            time.hours = ((data[3] & 0x3e) >> 1) as u32;
            time.minutes = (((data[3] & 0x01) << 5) | (data[4] >> 3)) as u32;
            time.seconds = Some((((data[4] & 0x03) << 3) | (data[5] >> 5)) as u32);
            payload_start = 6;
        }
        _ => return None,
    }

    let a_flag = (data[0] & 0x80) != 0;
    let g_flag = (data[0] & 0x40) != 0;
    let p_flag = (data[0] & 0x20) != 0;
    let product_id = (((data[0] & 0x1f) as u32) << 6) | ((data[1] >> 2) as u32);
    let s_flag = (data[1] & 0x02) != 0;

    let payload = data[payload_start..].to_vec();
    let reports = if dlac::is_dlac_text(product_id) {
        dlac::split_text_reports(&dlac::decode_dlac(&payload))
            .into_iter()
            .map(SerReport::from)
            .collect()
    } else {
        Vec::new()
    };

    Some(FisbProduct {
        product_id,
        product_name: dlac::product_name(product_id).to_string(),
        a_flag,
        g_flag,
        p_flag,
        s_flag,
        time,
        data: payload,
        reports,
    })
}

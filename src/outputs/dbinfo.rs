//! Static identity enrichment: country from ICAO hex allocation blocks
//! and MMSI MID prefixes (ITU/ICAO public allocation facts — curated
//! common subsets, `None` when unknown), plus an optional user-supplied
//! aircraft database (tar1090/Mictronics CSV: icao;reg;type;...).

use std::collections::HashMap;
use std::sync::OnceLock;

/// Country for an ICAO 24-bit address (major allocation blocks).
pub fn icao_country(icao: u32) -> Option<&'static str> {
    const BLOCKS: &[(u32, u32, &str)] = &[
        (0x004000, 0x0043FF, "Zimbabwe"),
        (0x006000, 0x006FFF, "Mozambique"),
        (0x008000, 0x00FFFF, "South Africa"),
        (0x010000, 0x017FFF, "Egypt"),
        (0x018000, 0x01FFFF, "Libya"),
        (0x020000, 0x027FFF, "Morocco"),
        (0x028000, 0x02FFFF, "Tunisia"),
        (0x0A0000, 0x0A7FFF, "Algeria"),
        (0x100000, 0x1FFFFF, "Russia"),
        (0x201000, 0x2013FF, "Cameroon"),
        (0x300000, 0x33FFFF, "Italy"),
        (0x340000, 0x37FFFF, "Spain"),
        (0x380000, 0x3BFFFF, "France"),
        (0x3C0000, 0x3FFFFF, "Germany"),
        (0x400000, 0x43FFFF, "United Kingdom"),
        (0x440000, 0x447FFF, "Austria"),
        (0x448000, 0x44FFFF, "Belgium"),
        (0x450000, 0x457FFF, "Bulgaria"),
        (0x458000, 0x45FFFF, "Denmark"),
        (0x460000, 0x467FFF, "Finland"),
        (0x468000, 0x46FFFF, "Greece"),
        (0x470000, 0x477FFF, "Hungary"),
        (0x478000, 0x47FFFF, "Norway"),
        (0x480000, 0x487FFF, "Netherlands"),
        (0x488000, 0x48FFFF, "Poland"),
        (0x490000, 0x497FFF, "Portugal"),
        (0x498000, 0x49FFFF, "Czechia"),
        (0x4A0000, 0x4A7FFF, "Romania"),
        (0x4A8000, 0x4AFFFF, "Sweden"),
        (0x4B0000, 0x4B7FFF, "Switzerland"),
        (0x4B8000, 0x4BFFFF, "Türkiye"),
        (0x4C0000, 0x4C7FFF, "Serbia"),
        (0x4C8000, 0x4C83FF, "Cyprus"),
        (0x4CA000, 0x4CAFFF, "Ireland"),
        (0x4D0000, 0x4D7FFF, "Malta"),
        (0x4CC000, 0x4CCFFF, "Iceland"),
        (0x500000, 0x5003FF, "San Marino"),
        (0x680000, 0x6803FF, "Bhutan"),
        (0x700000, 0x700FFF, "Afghanistan"),
        (0x710000, 0x717FFF, "Saudi Arabia"),
        (0x718000, 0x71FFFF, "South Korea"),
        (0x720000, 0x727FFF, "North Korea"),
        (0x728000, 0x72FFFF, "Iraq"),
        (0x730000, 0x737FFF, "Iran"),
        (0x738000, 0x73FFFF, "Israel"),
        (0x740000, 0x747FFF, "Jordan"),
        (0x748000, 0x74FFFF, "Lebanon"),
        (0x750000, 0x757FFF, "Malaysia"),
        (0x758000, 0x75FFFF, "Philippines"),
        (0x760000, 0x767FFF, "Pakistan"),
        (0x768000, 0x76FFFF, "Singapore"),
        (0x770000, 0x777FFF, "Sri Lanka"),
        (0x778000, 0x77FFFF, "Syria"),
        (0x780000, 0x7BFFFF, "China"),
        (0x7C0000, 0x7FFFFF, "Australia"),
        (0x800000, 0x83FFFF, "India"),
        (0x840000, 0x87FFFF, "Japan"),
        (0x880000, 0x887FFF, "Thailand"),
        (0x888000, 0x88FFFF, "Viet Nam"),
        (0x8A0000, 0x8A7FFF, "Indonesia"),
        (0x900000, 0x9FFFFF, "(reserved)"),
        (0xA00000, 0xAFFFFF, "United States"),
        (0xC00000, 0xC3FFFF, "Canada"),
        (0xC80000, 0xC87FFF, "New Zealand"),
        (0xE00000, 0xE3FFFF, "Argentina"),
        (0xE40000, 0xE7FFFF, "Brazil"),
        (0xE80000, 0xE80FFF, "Chile"),
        (0x0D0000, 0x0D7FFF, "Mexico"),
    ];
    BLOCKS
        .iter()
        .find(|(lo, hi, _)| (*lo..=*hi).contains(&icao))
        .map(|(_, _, c)| *c)
}

/// Country for an MMSI (maritime identification digits).
pub fn mid_country(mmsi: u32) -> Option<&'static str> {
    // Base stations (00MIDxxxx) and regular ships (MIDxxxxxx).
    let mid = if mmsi < 10_000_000 {
        // 00MIDxxxx coast station form
        (mmsi / 10_000) as u16
    } else {
        (mmsi / 1_000_000) as u16
    };
    const MIDS: &[(u16, &str)] = &[
        (201, "Albania"), (203, "Austria"), (205, "Belgium"), (209, "Cyprus"),
        (210, "Cyprus"), (211, "Germany"), (212, "Cyprus"), (215, "Malta"),
        (218, "Germany"), (219, "Denmark"), (220, "Denmark"), (224, "Spain"),
        (225, "Spain"), (226, "France"), (227, "France"), (228, "France"),
        (229, "Malta"), (230, "Finland"), (231, "Faroe Is"), (232, "United Kingdom"),
        (233, "United Kingdom"), (234, "United Kingdom"), (235, "United Kingdom"),
        (236, "Gibraltar"), (237, "Greece"), (238, "Croatia"), (239, "Greece"),
        (240, "Greece"), (241, "Greece"), (242, "Morocco"), (243, "Hungary"),
        (244, "Netherlands"), (245, "Netherlands"), (246, "Netherlands"),
        (247, "Italy"), (248, "Malta"), (249, "Malta"), (250, "Ireland"),
        (251, "Iceland"), (252, "Liechtenstein"), (253, "Luxembourg"),
        (255, "Portugal (Madeira)"), (256, "Malta"), (257, "Norway"),
        (258, "Norway"), (259, "Norway"), (261, "Poland"), (263, "Portugal"),
        (264, "Romania"), (265, "Sweden"), (266, "Sweden"), (267, "Slovakia"),
        (268, "San Marino"), (269, "Switzerland"), (271, "Türkiye"),
        (272, "Ukraine"), (273, "Russia"), (274, "North Macedonia"),
        (275, "Latvia"), (276, "Estonia"), (277, "Lithuania"), (278, "Slovenia"),
        (279, "Serbia"), (301, "Anguilla"), (303, "United States (Alaska)"),
        (304, "Antigua and Barbuda"), (305, "Antigua and Barbuda"),
        (306, "Curaçao"), (307, "Aruba"), (308, "Bahamas"), (309, "Bahamas"),
        (310, "Bermuda"), (311, "Bahamas"), (316, "Canada"),
        (319, "Cayman Is"), (321, "Costa Rica"), (323, "Cuba"),
        (325, "Dominica"), (327, "Dominican Rep"), (329, "Guadeloupe"),
        (330, "Grenada"), (331, "Greenland"), (332, "Guatemala"),
        (334, "Honduras"), (336, "Haiti"), (338, "United States"),
        (339, "Jamaica"), (341, "St Kitts and Nevis"), (343, "St Lucia"),
        (345, "Mexico"), (347, "Martinique"), (348, "Montserrat"),
        (350, "Nicaragua"), (351, "Panama"), (352, "Panama"), (353, "Panama"),
        (354, "Panama"), (355, "Panama"), (356, "Panama"), (357, "Panama"),
        (358, "Puerto Rico"), (359, "El Salvador"), (361, "St Pierre and Miquelon"),
        (362, "Trinidad and Tobago"), (364, "Turks and Caicos"),
        (366, "United States"), (367, "United States"), (368, "United States"),
        (369, "United States"), (370, "Panama"), (371, "Panama"),
        (372, "Panama"), (373, "Panama"), (374, "Panama"), (375, "St Vincent"),
        (376, "St Vincent"), (377, "St Vincent"), (378, "British Virgin Is"),
        (379, "US Virgin Is"), (401, "Afghanistan"), (403, "Saudi Arabia"),
        (405, "Bangladesh"), (408, "Bahrain"), (410, "Bhutan"),
        (412, "China"), (413, "China"), (414, "China"), (416, "Taiwan"),
        (417, "Sri Lanka"), (419, "India"), (422, "Iran"), (423, "Azerbaijan"),
        (425, "Iraq"), (428, "Israel"), (431, "Japan"), (432, "Japan"),
        (434, "Turkmenistan"), (436, "Kazakhstan"), (437, "Uzbekistan"),
        (438, "Jordan"), (440, "South Korea"), (441, "South Korea"),
        (443, "Palestine"), (445, "North Korea"), (447, "Kuwait"),
        (450, "Lebanon"), (451, "Kyrgyzstan"), (453, "Macao"),
        (455, "Maldives"), (457, "Mongolia"), (459, "Nepal"), (461, "Oman"),
        (463, "Pakistan"), (466, "Qatar"), (468, "Syria"),
        (470, "United Arab Emirates"), (471, "United Arab Emirates"),
        (472, "Tajikistan"), (473, "Yemen"), (475, "Yemen"),
        (477, "Hong Kong"), (478, "Bosnia and Herzegovina"),
        (501, "Adelie Land"), (503, "Australia"), (506, "Myanmar"),
        (508, "Brunei"), (510, "Micronesia"), (511, "Palau"),
        (512, "New Zealand"), (514, "Cambodia"), (515, "Cambodia"),
        (516, "Christmas Is"), (518, "Cook Is"), (520, "Fiji"),
        (525, "Indonesia"), (529, "Kiribati"), (531, "Laos"),
        (533, "Malaysia"), (536, "Northern Mariana Is"), (538, "Marshall Is"),
        (540, "New Caledonia"), (542, "Niue"), (544, "Nauru"),
        (546, "French Polynesia"), (548, "Philippines"), (553, "Papua New Guinea"),
        (555, "Pitcairn"), (557, "Solomon Is"), (559, "American Samoa"),
        (561, "Samoa"), (563, "Singapore"), (564, "Singapore"),
        (565, "Singapore"), (566, "Singapore"), (567, "Thailand"),
        (570, "Tonga"), (572, "Tuvalu"), (574, "Viet Nam"),
        (576, "Vanuatu"), (577, "Vanuatu"), (578, "Wallis and Futuna"),
        (601, "South Africa"), (603, "Angola"), (605, "Algeria"),
        (607, "St Paul and Amsterdam Is"), (608, "Ascension Is"),
        (609, "Burundi"), (610, "Benin"), (611, "Botswana"),
        (612, "Central African Rep"), (613, "Cameroon"), (615, "Congo"),
        (616, "Comoros"), (617, "Cabo Verde"), (618, "Crozet Is"),
        (619, "Côte d'Ivoire"), (621, "Djibouti"), (622, "Egypt"),
        (624, "Ethiopia"), (625, "Eritrea"), (626, "Gabon"), (627, "Ghana"),
        (629, "Gambia"), (630, "Guinea-Bissau"), (631, "Equatorial Guinea"),
        (632, "Guinea"), (633, "Burkina Faso"), (634, "Kenya"),
        (635, "Kerguelen Is"), (636, "Liberia"), (637, "Liberia"),
        (638, "South Sudan"), (642, "Libya"), (644, "Lesotho"),
        (645, "Mauritius"), (647, "Madagascar"), (649, "Mali"),
        (650, "Mozambique"), (654, "Mauritania"), (655, "Malawi"),
        (656, "Niger"), (657, "Nigeria"), (659, "Namibia"),
        (660, "Réunion"), (661, "Rwanda"), (662, "Sudan"), (663, "Senegal"),
        (664, "Seychelles"), (665, "St Helena"), (666, "Somalia"),
        (667, "Sierra Leone"), (668, "São Tomé and Príncipe"),
        (669, "Eswatini"), (670, "Chad"), (671, "Togo"), (672, "Tunisia"),
        (674, "Tanzania"), (675, "Uganda"), (676, "DR Congo"),
        (677, "Tanzania"), (678, "Zambia"), (679, "Zimbabwe"),
        (701, "Argentina"), (710, "Brazil"), (720, "Bolivia"),
        (725, "Chile"), (730, "Colombia"), (735, "Ecuador"),
        (740, "Falkland Is"), (745, "Guiana"), (750, "Guyana"),
        (755, "Paraguay"), (760, "Peru"), (765, "Suriname"),
        (770, "Uruguay"), (775, "Venezuela"),
    ];
    MIDS.iter().find(|(m, _)| *m == mid).map(|(_, c)| *c)
}

/// Optional aircraft database: tar1090 / Mictronics CSV
/// (`icao;registration;type;...`, also accepts comma separators).
pub struct AircraftDb {
    by_icao: HashMap<u32, (String, String)>,
}

static AIRCRAFT_DB: OnceLock<AircraftDb> = OnceLock::new();

impl AircraftDb {
    pub fn load(path: &std::path::Path) -> anyhow::Result<usize> {
        let text = std::fs::read_to_string(path)?;
        let mut by_icao = HashMap::new();
        for line in text.lines() {
            let sep = if line.contains(';') { ';' } else { ',' };
            let mut f = line.split(sep);
            let (Some(hex), reg, typ) = (f.next(), f.next(), f.next()) else { continue };
            let Ok(icao) = u32::from_str_radix(hex.trim(), 16) else { continue };
            by_icao.insert(
                icao,
                (
                    reg.unwrap_or("").trim().to_string(),
                    typ.unwrap_or("").trim().to_string(),
                ),
            );
        }
        let n = by_icao.len();
        let _ = AIRCRAFT_DB.set(AircraftDb { by_icao });
        Ok(n)
    }

    pub fn lookup(icao: u32) -> Option<(&'static str, &'static str)> {
        let db = AIRCRAFT_DB.get()?;
        db.by_icao.get(&icao).map(|(r, t)| (r.as_str(), t.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icao_blocks() {
        assert_eq!(icao_country(0xA9B27C), Some("United States"));
        assert_eq!(icao_country(0x4D2023), Some("Malta"));
        assert_eq!(icao_country(0x3C5EF2), Some("Germany"));
        assert_eq!(icao_country(0x7C6B2D), Some("Australia"));
    }

    #[test]
    fn mids() {
        assert_eq!(mid_country(366_123_456), Some("United States"));
        assert_eq!(mid_country(3_669_146), Some("United States")); // 00MIDxxxx coast station
        assert_eq!(mid_country(232_001_234), Some("United Kingdom"));
    }
}

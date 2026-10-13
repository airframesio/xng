//! Dashboard basemap: what the map draws.
//!
//! The default is OpenFreeMap (openfreemap.org): OpenStreetMap data as vector
//! tiles, MIT-licensed, with no API keys, accounts or request limits, and
//! self-hostable. The dashboard renders it with MapLibre beneath its Leaflet
//! overlays and loads it straight from the style's host; xng fetches nothing
//! itself. Any MapLibre style can be used instead via `style-url` (e.g. a
//! self-hosted OpenFreeMap). OpenStreetMap raster tiles are the fallback.

use serde_json::json;

/// Raster or vector (MapLibre) basemap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapFormat {
    #[default]
    Vector,
    Raster,
}

/// Basemap settings (`[outputs.map]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapConfig {
    /// MapLibre style URL (vector).
    pub style_url: String,
    pub format: MapFormat,
}

/// OpenFreeMap's public styles.
const STYLES: [&str; 5] = ["dark", "positron", "bright", "liberty", "fiord"];

const OPENFREEMAP_ATTRIBUTION: &str = "<a href=\"https://openfreemap.org\">OpenFreeMap</a> <a href=\"https://www.openmaptiles.org/\">&copy; OpenMapTiles</a> Data from <a href=\"https://www.openstreetmap.org/copyright\">OpenStreetMap</a>";
const OSM_ATTRIBUTION: &str = "&copy; <a href=\"https://www.openstreetmap.org/copyright\">OpenStreetMap</a> contributors";
const OSM_TILES: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

/// URL of an OpenFreeMap public style.
fn openfreemap(style: &str) -> String {
    format!("https://tiles.openfreemap.org/styles/{style}")
}

impl Default for MapConfig {
    /// OpenFreeMap `dark`, vector.
    fn default() -> Self {
        Self { style_url: openfreemap("dark"), format: MapFormat::Vector }
    }
}

impl MapConfig {
    /// Build from config: an OpenFreeMap `style` name, or any MapLibre
    /// `style_url` (which takes precedence), and `vector`/`raster`.
    pub fn new(style: Option<&str>, style_url: Option<&str>, format: Option<&str>) -> anyhow::Result<Self> {
        let style_url = match (style_url, style) {
            (Some(u), _) => {
                anyhow::ensure!(
                    (u.starts_with("https://") || u.starts_with("http://")) && !u.contains(char::is_whitespace),
                    "map style-url `{u}` is not an http(s) URL"
                );
                u.to_string()
            }
            (None, Some(s)) => {
                anyhow::ensure!(STYLES.contains(&s), "map style `{s}` is not one of: {}", STYLES.join(", "));
                openfreemap(s)
            }
            (None, None) => openfreemap("dark"),
        };
        let format = match format.unwrap_or("vector") {
            "vector" => MapFormat::Vector,
            "raster" => MapFormat::Raster,
            other => anyhow::bail!("map format `{other}` is not `vector` or `raster`"),
        };
        Ok(Self { style_url, format })
    }

    /// What the dashboard should draw (`/map/config.json`). The OSM raster
    /// layer is also what the page falls back to when vector can't render.
    pub fn client_json(&self) -> String {
        let fallback = json!({ "tiles": OSM_TILES, "attribution": OSM_ATTRIBUTION, "maxZoom": 19 });
        match self.format {
            MapFormat::Vector => {
                let attribution = if self.style_url.starts_with("https://tiles.openfreemap.org/") {
                    OPENFREEMAP_ATTRIBUTION
                } else {
                    // Custom style: its own sources carry the attribution.
                    ""
                };
                json!({ "format": "vector", "style": self.style_url, "attribution": attribution, "fallback": fallback })
            }
            MapFormat::Raster => json!({ "format": "raster", "fallback": fallback }),
        }
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The parsed `/map/config.json` body for `c`.
    fn client(c: &MapConfig) -> Value {
        serde_json::from_str(&c.client_json()).unwrap()
    }

    /// No config means OpenFreeMap dark as a vector map, with attribution.
    #[test]
    fn defaults_to_openfreemap_dark_vector() {
        let c = MapConfig::default();
        assert_eq!(c, MapConfig::new(None, None, None).unwrap());
        let j = client(&c);
        assert_eq!(j["format"], "vector");
        assert_eq!(j["style"], "https://tiles.openfreemap.org/styles/dark");
        assert!(j["attribution"].as_str().unwrap().contains("OpenStreetMap"));
    }

    /// Every named style maps to its OpenFreeMap URL; unknown names and
    /// formats are rejected.
    #[test]
    fn named_styles_and_validation() {
        for s in STYLES {
            let j = client(&MapConfig::new(Some(s), None, None).unwrap());
            assert_eq!(j["style"], format!("https://tiles.openfreemap.org/styles/{s}"));
        }
        assert!(MapConfig::new(Some("satellite"), None, None).is_err());
        assert!(MapConfig::new(None, None, Some("svg")).is_err());
    }

    /// `style-url` wins over `style`, carries no attribution of its own, and
    /// must be an http(s) URL.
    #[test]
    fn style_url_overrides_and_is_validated() {
        let c = MapConfig::new(Some("positron"), Some("https://tiles.example.org/styles/dark"), None).unwrap();
        let j = client(&c);
        assert_eq!(j["style"], "https://tiles.example.org/styles/dark");
        assert_eq!(j["attribution"], "", "custom styles carry their own attribution");
        assert!(MapConfig::new(None, Some("javascript:alert(1)"), None).is_err());
        assert!(MapConfig::new(None, Some("tiles.example.org/style"), None).is_err());
        assert!(MapConfig::new(None, Some("https://x/ y"), None).is_err());
    }

    /// Raster draws OpenStreetMap tiles, which are also every config's
    /// fallback.
    #[test]
    fn raster_and_fallback_use_osm() {
        let j = client(&MapConfig::new(None, None, Some("raster")).unwrap());
        assert_eq!(j["format"], "raster");
        assert_eq!(j["fallback"]["tiles"], OSM_TILES);
        assert_eq!(client(&MapConfig::default())["fallback"]["tiles"], OSM_TILES);
    }
}

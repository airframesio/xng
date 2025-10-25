use core::fmt;

use lazy_static::lazy_static;
use regex::{Regex, Captures};
use serde::{Deserialize, Serialize, Serializer, Deserializer};
use serde::de;

struct WKTPointVisitor;

#[derive(Clone, Debug)]
pub struct WKTPoint {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl WKTPoint {
    pub fn valid(&self) -> bool {
        self.x > -180.0 && self.x < 180.0 && self.y > -90.0 && self.y < 90.0
    }

    pub fn as_tuple(&self) -> (f64, f64, f64) {
        (self.x, self.y, self.z)
    }    
}

fn parse_point_from_matches(m: Captures) -> Option<(f64, f64, f64)> {
        let Ok(x) = m.get(1).map_or("", |v| v.as_str()).parse::<f64>() else {
            return None
        };
        let Ok(y) = m.get(2).map_or("", |v| v.as_str()).parse::<f64>() else {
            return None
        };
        let Ok(z) = m.get(3).map_or("", |v| v.as_str()).parse::<f64>() else {
            return None
        };

    Some((x, y, z))    
}

impl Serialize for WKTPoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("POINT ({} {} {})", self.x, self.y, self.z))
    }
}

impl<'de> de::Visitor<'de> for WKTPointVisitor {
    type Value = WKTPoint;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a WKT POINT string in the format of \"POINT (x y z)\"")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        lazy_static! {
            static ref POINT_FMT: Regex = Regex::new(
                r"(?x)
                POINT\s*\(\s*((?:|-)[0-9]+(?:|\.)[0-9]*)\s+((?:|-)[0-9]+(?:|\.)[0-9]*)\s+((?:|-)[0-9]+(?:|\.)[0-9]*)\s*\)
                "
            ).unwrap();
        }
        
        let Some(m) = POINT_FMT.captures(s) else {
            return Err(de::Error::invalid_type(de::Unexpected::Str(s), &self)) 
        };

        let Some((x, y, z)) = parse_point_from_matches(m) else {
            return Err(de::Error::invalid_value(de::Unexpected::Str(s), &self))
        };
             
        Ok(WKTPoint { x, y, z })
    }
}


impl<'de> Deserialize<'de> for WKTPoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(WKTPointVisitor)
    }
}

struct WKTPolylineVisitor;

#[derive(Debug)]
pub struct WKTPolyline {
    pub points: Vec<(f64, f64, f64)>,
}

impl Serialize for WKTPolyline {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!(
            "LINESTRING ({})",
            self.points
                .iter()
                .map(|p| format!("{} {} {}", p.0, p.1, p.2))
                .collect::<Vec<String>>()
                .join(", "),
        ))
    }
}

impl<'de> de::Visitor<'de> for WKTPolylineVisitor {
    type Value = WKTPolyline;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a WKT LINESTRING string in the format of \"LINESTRING (x y z, x1 y1 y1, ...)\"")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        lazy_static! {
            static ref LINE_FMT: Regex = Regex::new(
                r"(?x)
                LINESTRING\s*\(\s*(.+)\s*\)
                "
            ).unwrap();
            static ref COORD_FMT: Regex = Regex::new(
                r"(?x)
                \s*((?:|-)[0-9]+(?:|\.)[0-9]*)\s+((?:|-)[0-9]+(?:|\.)[0-9]*)\s+((?:|-)[0-9]+(?:|\.)[0-9]*)\s*
                "
            ).unwrap();
        }

        let Some(m) = LINE_FMT.captures(s) else {
            return Err(de::Error::invalid_type(de::Unexpected::Str(s), &self)) 
        };

        let mut points: Vec<(f64, f64, f64)> = Vec::new();
        for coord in m.get(1).map_or("", |v| v.as_str()).split(",").into_iter() {
            let Some(m) = COORD_FMT.captures(coord) else {
                return Err(de::Error::invalid_type(de::Unexpected::Str(s), &self)) 
            };

            let Some((x, y, z)) = parse_point_from_matches(m) else {
                return Err(de::Error::invalid_value(de::Unexpected::Str(s), &self))
            };

            points.push((x, y, z));
        }

        Ok(WKTPolyline { points }) 
    }
}

impl<'de> Deserialize<'de> for WKTPolyline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(WKTPolylineVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_wkt_point_valid() {
        let point = WKTPoint { x: -122.5, y: 37.8, z: 100.0 };
        assert!(point.valid());
    }

    #[test]
    fn test_wkt_point_invalid_longitude() {
        let point = WKTPoint { x: 200.0, y: 37.8, z: 100.0 };
        assert!(!point.valid());

        let point = WKTPoint { x: -200.0, y: 37.8, z: 100.0 };
        assert!(!point.valid());
    }

    #[test]
    fn test_wkt_point_invalid_latitude() {
        let point = WKTPoint { x: -122.5, y: 95.0, z: 100.0 };
        assert!(!point.valid());

        let point = WKTPoint { x: -122.5, y: -95.0, z: 100.0 };
        assert!(!point.valid());
    }

    #[test]
    fn test_wkt_point_as_tuple() {
        let point = WKTPoint { x: -122.5, y: 37.8, z: 100.0 };
        assert_eq!(point.as_tuple(), (-122.5, 37.8, 100.0));
    }

    #[test]
    fn test_wkt_point_serialize() {
        let point = WKTPoint { x: -122.5, y: 37.8, z: 100.0 };
        let serialized = serde_json::to_string(&point).unwrap();
        assert_eq!(serialized, "\"POINT (-122.5 37.8 100)\"");
    }

    #[test]
    fn test_wkt_point_deserialize() {
        let json = "\"POINT (-122.5 37.8 100)\"";
        let point: WKTPoint = serde_json::from_str(json).unwrap();
        assert_eq!(point.x, -122.5);
        assert_eq!(point.y, 37.8);
        assert_eq!(point.z, 100.0);
    }

    #[test]
    fn test_wkt_point_deserialize_with_spaces() {
        let json = "\"POINT (  -122.5   37.8   100  )\"";
        let point: WKTPoint = serde_json::from_str(json).unwrap();
        assert_eq!(point.x, -122.5);
        assert_eq!(point.y, 37.8);
        assert_eq!(point.z, 100.0);
    }

    #[test]
    fn test_wkt_point_deserialize_negative_coords() {
        let json = "\"POINT (-122.5 -37.8 -100)\"";
        let point: WKTPoint = serde_json::from_str(json).unwrap();
        assert_eq!(point.x, -122.5);
        assert_eq!(point.y, -37.8);
        assert_eq!(point.z, -100.0);
    }

    #[test]
    fn test_wkt_point_deserialize_integers() {
        let json = "\"POINT (122 37 100)\"";
        let point: WKTPoint = serde_json::from_str(json).unwrap();
        assert_eq!(point.x, 122.0);
        assert_eq!(point.y, 37.0);
        assert_eq!(point.z, 100.0);
    }

    #[test]
    fn test_wkt_point_deserialize_invalid_format() {
        let json = "\"INVALID (-122.5 37.8 100)\"";
        let result: Result<WKTPoint, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_wkt_polyline_serialize() {
        let polyline = WKTPolyline {
            points: vec![(-122.5, 37.8, 100.0), (-122.6, 37.9, 150.0)],
        };
        let serialized = serde_json::to_string(&polyline).unwrap();
        assert_eq!(serialized, "\"LINESTRING (-122.5 37.8 100, -122.6 37.9 150)\"");
    }

    #[test]
    fn test_wkt_polyline_deserialize() {
        let json = "\"LINESTRING (-122.5 37.8 100, -122.6 37.9 150)\"";
        let polyline: WKTPolyline = serde_json::from_str(json).unwrap();
        assert_eq!(polyline.points.len(), 2);
        assert_eq!(polyline.points[0], (-122.5, 37.8, 100.0));
        assert_eq!(polyline.points[1], (-122.6, 37.9, 150.0));
    }

    #[test]
    fn test_wkt_polyline_deserialize_single_point() {
        let json = "\"LINESTRING (-122.5 37.8 100)\"";
        let polyline: WKTPolyline = serde_json::from_str(json).unwrap();
        assert_eq!(polyline.points.len(), 1);
        assert_eq!(polyline.points[0], (-122.5, 37.8, 100.0));
    }

    #[test]
    fn test_wkt_polyline_deserialize_multiple_points() {
        let json = "\"LINESTRING (-122.5 37.8 100, -122.6 37.9 150, -122.7 38.0 200)\"";
        let polyline: WKTPolyline = serde_json::from_str(json).unwrap();
        assert_eq!(polyline.points.len(), 3);
        assert_eq!(polyline.points[0], (-122.5, 37.8, 100.0));
        assert_eq!(polyline.points[1], (-122.6, 37.9, 150.0));
        assert_eq!(polyline.points[2], (-122.7, 38.0, 200.0));
    }

    #[test]
    fn test_wkt_polyline_deserialize_invalid_format() {
        let json = "\"INVALID (-122.5 37.8 100, -122.6 37.9 150)\"";
        let result: Result<WKTPolyline, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}


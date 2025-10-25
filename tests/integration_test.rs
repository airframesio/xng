// Integration tests for XNG
// These tests verify that the major components work together correctly

#[cfg(test)]
mod tests {
    use xng::common::wkt::{WKTPoint, WKTPolyline};
    use serde_json;

    #[test]
    fn test_wkt_round_trip_serialization() {
        // Test that WKT types can be serialized and deserialized correctly
        let point = WKTPoint { x: -122.5, y: 37.8, z: 100.0 };
        let serialized = serde_json::to_string(&point).unwrap();
        let deserialized: WKTPoint = serde_json::from_str(&serialized).unwrap();

        assert_eq!(point.x, deserialized.x);
        assert_eq!(point.y, deserialized.y);
        assert_eq!(point.z, deserialized.z);
    }

    #[test]
    fn test_wkt_polyline_round_trip() {
        let polyline = WKTPolyline {
            points: vec![
                (-122.5, 37.8, 100.0),
                (-122.6, 37.9, 150.0),
                (-122.7, 38.0, 200.0),
            ],
        };

        let serialized = serde_json::to_string(&polyline).unwrap();
        let deserialized: WKTPolyline = serde_json::from_str(&serialized).unwrap();

        assert_eq!(polyline.points.len(), deserialized.points.len());
        for (i, point) in polyline.points.iter().enumerate() {
            assert_eq!(point, &deserialized.points[i]);
        }
    }
}

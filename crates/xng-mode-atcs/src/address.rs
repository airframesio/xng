//! ATCS address decode (AAR Spec-200).
//!
//! An ATCS address is a string of BCD digits. The **first digit** is the
//! user-group / direction identifier; the **next three digits** are the
//! AAR-assigned railroad number; the remaining digits are the
//! railroad-internal routing — a line / codeline / territory field and a
//! node (serial MCP) field.
//!
//! User-group identifiers (AAR Standard Manual of ATCS; ATCS Monitor
//! address documentation):
//!
//! | Digit | Meaning                                     |
//! |-------|---------------------------------------------|
//! | 0     | Network applications (ground network)       |
//! | 1     | Locomotive applications                      |
//! | 2     | Host applications (office / dispatch)        |
//! | 3     | Wayside equipment — wireline connected       |
//! | 4     | Other mobiles                                |
//! | 5     | Wayside equipment — RF connected (field MCP) |
//!
//! The exact line/node split is railroad- and address-type-specific
//! (e.g. type-7 random-access uses `T-RRR-CC-AAA`: 3-digit codeline +
//! 3-digit serial node; type-5 MCP uses `T-RRR-XX-AAAA`: 2-digit
//! extension + 4-digit serial node). Rather than fabricate a single
//! unverifiable split, this decoder exposes the verbatim digit string and
//! the two externally-fixed fields — type and railroad — plus the
//! remaining routing digits, and offers a best-effort line/node split for
//! the two documented type formats.

use serde::Serialize;

/// The user-group / direction class given by an ATCS address's first digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressType {
    /// 0 — network applications (ground network).
    Network,
    /// 1 — locomotive applications.
    Locomotive,
    /// 2 — host applications (office / dispatch).
    Host,
    /// 3 — wayside equipment, wireline connected.
    WaysideWireline,
    /// 4 — other mobiles.
    OtherMobile,
    /// 5 — wayside equipment, RF connected (field MCP).
    WaysideRf,
    /// Any other leading digit (6–9), reserved / railroad-specific.
    Other(u8),
}

impl AddressType {
    pub fn from_digit(d: u8) -> Self {
        match d {
            0 => AddressType::Network,
            1 => AddressType::Locomotive,
            2 => AddressType::Host,
            3 => AddressType::WaysideWireline,
            4 => AddressType::OtherMobile,
            5 => AddressType::WaysideRf,
            other => AddressType::Other(other),
        }
    }

    /// True for the two field (wayside / MCP) directions.
    pub fn is_field(self) -> bool {
        matches!(self, AddressType::WaysideWireline | AddressType::WaysideRf)
    }

    /// True for the ground-side directions (network / host / office).
    pub fn is_ground(self) -> bool {
        matches!(self, AddressType::Network | AddressType::Host)
    }

    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            AddressType::Network => "network",
            AddressType::Locomotive => "locomotive",
            AddressType::Host => "host/office",
            AddressType::WaysideWireline => "wayside-wireline",
            AddressType::OtherMobile => "other-mobile",
            AddressType::WaysideRf => "wayside-rf/mcp",
            AddressType::Other(_) => "other",
        }
    }
}

/// A decoded ATCS address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AtcsAddress {
    /// The verbatim BCD digit string, e.g. "2125385538".
    pub digits: String,
    /// User-group / direction from the first digit.
    pub addr_type: AddressType,
    /// AAR-assigned railroad number (digits 2..4), e.g. 802 for UPRR.
    pub railroad: u16,
    /// The routing digits after the railroad number (line/territory + node),
    /// verbatim. Empty if the address is only type + railroad.
    pub routing: String,
    /// Best-effort line / codeline / territory field, for the two
    /// documented address-type formats (type 5: 2-digit extension;
    /// type 7: 3-digit codeline). `None` when the format is not one of
    /// those documented splits.
    pub line: Option<u16>,
    /// Best-effort node / serial-MCP field, paired with `line`.
    pub node: Option<u32>,
}

impl AtcsAddress {
    /// Parse a BCD digit string into an ATCS address. Returns `None` if the
    /// string is empty or contains a non-digit.
    pub fn parse(digits: &str) -> Option<Self> {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let bytes = digits.as_bytes();
        let addr_type = AddressType::from_digit(bytes[0] - b'0');

        // Railroad is digits 2..4 (the three after the type digit), when
        // present; shorter addresses just carry what they have.
        let railroad = if bytes.len() >= 4 {
            digits[1..4].parse::<u16>().ok()?
        } else {
            digits[1..].parse::<u16>().unwrap_or(0)
        };

        let routing = if bytes.len() > 4 {
            digits[4..].to_string()
        } else {
            String::new()
        };

        // Documented line/node splits by leading digit and total length.
        let (line, node) = split_line_node(addr_type, &routing);

        Some(AtcsAddress {
            digits: digits.to_string(),
            addr_type,
            railroad,
            routing,
            line,
            node,
        })
    }
}

/// Apply the documented `T-RRR-XX-AAAA` (type 5) and `T-RRR-CC-AAA`
/// (type 7) splits to the routing digits. Other formats return `None`s.
fn split_line_node(addr_type: AddressType, routing: &str) -> (Option<u16>, Option<u32>) {
    match addr_type {
        // Type 5 MCP: 2-digit extension/line + 4-digit serial node.
        AddressType::WaysideRf if routing.len() == 6 => {
            let line = routing[0..2].parse::<u16>().ok();
            let node = routing[2..6].parse::<u32>().ok();
            (line, node)
        }
        // Type "7" random-access (Other(7)): 3-digit codeline + 3-digit node.
        AddressType::Other(7) if routing.len() == 6 => {
            let line = routing[0..3].parse::<u16>().ok();
            let node = routing[3..6].parse::<u32>().ok();
            (line, node)
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anchored to the sigidwiki.com decoded Spec-200 sample:
    ///   "To Dispatch: 2125385538"  (a Host / dispatch address)
    /// Leading digit 2 = Host (office/dispatch); the three digits after
    /// the type digit are the AAR railroad number = 125.
    #[test]
    fn host_dispatch_address_from_sigidwiki_sample() {
        let a = AtcsAddress::parse("2125385538").unwrap();
        assert_eq!(a.addr_type, AddressType::Host);
        assert!(a.addr_type.is_ground());
        assert!(!a.addr_type.is_field());
        assert_eq!(a.railroad, 125);
        assert_eq!(a.routing, "385538");
    }

    /// Anchored to the same sigidwiki.com sample:
    ///   "Wayside Device - RF: 5125013826"  (a field MCP address)
    /// Leading digit 5 = Wayside-RF (field MCP); railroad = 125 (the same
    /// railroad as the dispatch peer above); the documented type-5 split
    /// gives a 2-digit line + 4-digit node.
    #[test]
    fn wayside_rf_address_from_sigidwiki_sample() {
        let a = AtcsAddress::parse("5125013826").unwrap();
        assert_eq!(a.addr_type, AddressType::WaysideRf);
        assert!(a.addr_type.is_field());
        assert!(!a.addr_type.is_ground());
        assert_eq!(a.railroad, 125);
        assert_eq!(a.routing, "013826");
        // Type-5 T-RRR-XX-AAAA split: XX=01, AAAA=3826.
        assert_eq!(a.line, Some(1));
        assert_eq!(a.node, Some(3826));
    }

    /// The user-group identifier table (AAR Standard Manual of ATCS).
    #[test]
    fn address_type_digit_table() {
        assert_eq!(AddressType::from_digit(0), AddressType::Network);
        assert_eq!(AddressType::from_digit(1), AddressType::Locomotive);
        assert_eq!(AddressType::from_digit(2), AddressType::Host);
        assert_eq!(AddressType::from_digit(3), AddressType::WaysideWireline);
        assert_eq!(AddressType::from_digit(4), AddressType::OtherMobile);
        assert_eq!(AddressType::from_digit(5), AddressType::WaysideRf);
        assert_eq!(AddressType::from_digit(7), AddressType::Other(7));
    }

    /// Type-7 random-access format T-RRR-CC-AAA: 3-digit codeline + 3-digit
    /// serial node (ATCS Monitor documented format).
    #[test]
    fn type7_random_access_split() {
        // 7-802-005-003 = railroad 802, codeline 005, node 003.
        let a = AtcsAddress::parse("7802005003").unwrap();
        assert_eq!(a.addr_type, AddressType::Other(7));
        assert_eq!(a.railroad, 802);
        assert_eq!(a.line, Some(5));
        assert_eq!(a.node, Some(3));
    }

    #[test]
    fn rejects_non_digits_and_empty() {
        assert!(AtcsAddress::parse("").is_none());
        assert!(AtcsAddress::parse("21x5").is_none());
    }
}

//! NMEA 0183 AIVDM sentence encoding (IEC 61162-1): 6-bit ASCII armoring
//! of the message bit string, fill bits, XOR checksum, multi-sentence
//! fragmentation for long messages.

/// Max armored payload characters per sentence (keeps the full sentence
/// within the traditional 82-character NMEA limit).
const MAX_PAYLOAD_CHARS: usize = 60;

/// Armor one 6-bit value.
fn armor(v: u8) -> char {
    (if v < 40 { v + 48 } else { v + 56 }) as char
}

/// De-armor one payload character (test/verification helper).
pub fn dearmor(c: char) -> u8 {
    let v = c as u8 - 48;
    if v > 39 {
        v - 8
    } else {
        v
    }
}

/// Expand an armored payload back into message bits (verification helper).
pub fn payload_to_bits(payload: &str) -> Vec<u8> {
    payload
        .chars()
        .flat_map(|c| {
            let v = dearmor(c);
            (0..6).rev().map(move |i| (v >> i) & 1)
        })
        .collect()
}

fn checksum(body: &str) -> u8 {
    body.bytes().fold(0, |c, b| c ^ b)
}

pub struct SentenceBuilder {
    seq: u8,
}

impl SentenceBuilder {
    pub fn new() -> Self {
        Self { seq: 0 }
    }

    /// Encode a message bit string as one or more AIVDM sentences.
    pub fn encode(&mut self, message_bits: &[u8], channel: char) -> Vec<String> {
        let fill = (6 - message_bits.len() % 6) % 6;
        let mut padded = message_bits.to_vec();
        padded.extend(std::iter::repeat(0).take(fill));
        let chars: String = padded
            .chunks_exact(6)
            .map(|c| armor(c.iter().fold(0u8, |v, &b| (v << 1) | b)))
            .collect();

        let fragments: Vec<&str> = chars
            .as_bytes()
            .chunks(MAX_PAYLOAD_CHARS)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        let total = fragments.len();
        let seq_field = if total > 1 {
            let s = self.seq.to_string();
            self.seq = (self.seq + 1) % 10;
            s
        } else {
            String::new()
        };

        fragments
            .iter()
            .enumerate()
            .map(|(i, frag)| {
                let frag_fill = if i == total - 1 { fill } else { 0 };
                let body = format!(
                    "AIVDM,{total},{},{seq_field},{channel},{frag},{frag_fill}",
                    i + 1
                );
                format!("!{body}*{:02X}", checksum(&body))
            })
            .collect()
    }
}

impl Default for SentenceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armor_roundtrip() {
        for v in 0..64u8 {
            assert_eq!(dearmor(armor(v)), v);
        }
    }

    #[test]
    fn single_sentence_shape() {
        let bits = vec![0u8; 168];
        let s = SentenceBuilder::new().encode(&bits, 'A');
        assert_eq!(s.len(), 1);
        assert!(s[0].starts_with("!AIVDM,1,1,,A,"));
        assert!(s[0].contains(",0*"));
    }

    #[test]
    fn long_message_fragments() {
        let bits = vec![0u8; 424]; // type 5 static data: 71 chars
        let s = SentenceBuilder::new().encode(&bits, 'B');
        assert_eq!(s.len(), 2);
        assert!(s[0].starts_with("!AIVDM,2,1,0,B,"));
        assert!(s[1].starts_with("!AIVDM,2,2,0,B,"));
    }
}

//! Minimal protobuf wire reader (enough for Meshtastic messages).

/// One field value as it appears on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Value<'a> {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Varint(v) | Value::Fixed64(v) => Some(*v),
            Value::Fixed32(v) => Some(*v as u64),
            Value::Bytes(_) => None,
        }
    }
    pub fn as_u32(&self) -> Option<u32> {
        self.as_u64().map(|v| v as u32)
    }
    /// `sfixed32` / `int32` reinterpretation.
    pub fn as_i32(&self) -> Option<i32> {
        self.as_u64().map(|v| v as u32 as i32)
    }
    /// `sint32` (zigzag).
    pub fn as_sint32(&self) -> Option<i32> {
        self.as_u64().map(|v| {
            let v = v as u32;
            ((v >> 1) as i32) ^ -((v & 1) as i32)
        })
    }
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Fixed32(v) => Some(f32::from_bits(*v)),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<String> {
        self.as_bytes()
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(str::to_string)
    }
}

fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        // The tenth byte holds only bit 63.
        if shift == 63 && b & 0x7F > 1 {
            return None;
        }
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Parse a whole message into (field number, value) pairs. `None` when the
/// bytes are not a well-formed protobuf message.
pub fn parse(buf: &[u8]) -> Option<Vec<(u32, Value<'_>)>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let key = varint(buf, &mut pos)?;
        // Field numbers are 1..2^29-1.
        if key >> 3 >= 1 << 29 {
            return None;
        }
        let field = (key >> 3) as u32;
        if field == 0 {
            return None;
        }
        let v = match key & 7 {
            0 => Value::Varint(varint(buf, &mut pos)?),
            1 => {
                let b = buf.get(pos..pos + 8)?;
                pos += 8;
                Value::Fixed64(u64::from_le_bytes(b.try_into().ok()?))
            }
            2 => {
                let len = varint(buf, &mut pos)? as usize;
                let b = buf.get(pos..pos.checked_add(len)?)?;
                pos += len;
                Value::Bytes(b)
            }
            5 => {
                let b = buf.get(pos..pos + 4)?;
                pos += 4;
                Value::Fixed32(u32::from_le_bytes(b.try_into().ok()?))
            }
            _ => return None,
        };
        out.push((field, v));
    }
    Some(out)
}

/// Accessor helpers over a parsed message.
pub struct Msg<'a>(pub Vec<(u32, Value<'a>)>);

impl<'a> Msg<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        parse(buf).map(Msg)
    }
    pub fn get(&self, f: u32) -> Option<&Value<'a>> {
        self.0.iter().rev().find(|(k, _)| *k == f).map(|(_, v)| v)
    }
    pub fn all(&self, f: u32) -> impl Iterator<Item = &Value<'a>> {
        self.0.iter().filter(move |(k, _)| *k == f).map(|(_, v)| v)
    }
    /// Repeated scalar field, packed or not. `width` 0 = varint, 4 = fixed32.
    pub fn repeated(&self, f: u32, width: usize) -> Vec<u64> {
        let mut out = Vec::new();
        for v in self.all(f) {
            match v {
                Value::Bytes(b) => {
                    if width == 4 {
                        out.extend(
                            b.chunks_exact(4)
                                .map(|c| u32::from_le_bytes(c.try_into().unwrap()) as u64),
                        );
                    } else {
                        let mut pos = 0;
                        while pos < b.len() {
                            match varint(b, &mut pos) {
                                Some(x) => out.push(x),
                                None => break,
                            }
                        }
                    }
                }
                other => {
                    if let Some(x) = other.as_u64() {
                        out.push(x);
                    }
                }
            }
        }
        out
    }
}

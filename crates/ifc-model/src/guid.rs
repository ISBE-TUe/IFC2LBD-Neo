const CONVERSION_TABLE: &[u8; 64] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuidParts {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

pub fn expand_ifc_guid(compressed: &str) -> Option<String> {
    let parts = parts_from_compressed(compressed)?;
    Some(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        parts.data1,
        parts.data2,
        parts.data3,
        parts.data4[0],
        parts.data4[1],
        parts.data4[2],
        parts.data4[3],
        parts.data4[4],
        parts.data4[5],
        parts.data4[6],
        parts.data4[7]
    ))
}

pub fn compress_uuid_string(uuid: &str) -> Option<String> {
    let parts = parts_from_uuid(uuid)?;
    compressed_from_parts(parts)
}

fn parts_from_uuid(uuid: &str) -> Option<GuidParts> {
    let normalized: String = uuid.chars().filter(|ch| *ch != '-').collect();
    if normalized.len() != 32 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    Some(GuidParts {
        data1: u32::from_str_radix(&normalized[0..8], 16).ok()?,
        data2: u16::from_str_radix(&normalized[8..12], 16).ok()?,
        data3: u16::from_str_radix(&normalized[12..16], 16).ok()?,
        data4: [
            u8::from_str_radix(&normalized[16..18], 16).ok()?,
            u8::from_str_radix(&normalized[18..20], 16).ok()?,
            u8::from_str_radix(&normalized[20..22], 16).ok()?,
            u8::from_str_radix(&normalized[22..24], 16).ok()?,
            u8::from_str_radix(&normalized[24..26], 16).ok()?,
            u8::from_str_radix(&normalized[26..28], 16).ok()?,
            u8::from_str_radix(&normalized[28..30], 16).ok()?,
            u8::from_str_radix(&normalized[30..32], 16).ok()?,
        ],
    })
}

fn compressed_from_parts(parts: GuidParts) -> Option<String> {
    let nums = [
        (parts.data1 / 16_777_216) as u32,
        parts.data1 % 16_777_216,
        (u32::from(parts.data2) * 256) + (u32::from(parts.data3) / 256),
        (u32::from(parts.data3 % 256) * 65_536)
            + (u32::from(parts.data4[0]) * 256)
            + u32::from(parts.data4[1]),
        (u32::from(parts.data4[2]) * 65_536)
            + (u32::from(parts.data4[3]) * 256)
            + u32::from(parts.data4[4]),
        (u32::from(parts.data4[5]) * 65_536)
            + (u32::from(parts.data4[6]) * 256)
            + u32::from(parts.data4[7]),
    ];

    let mut result = String::with_capacity(22);
    for (index, number) in nums.into_iter().enumerate() {
        let len = if index == 0 { 2 } else { 4 };
        encode_base64_fixed(number, len, &mut result)?;
    }
    Some(result)
}

fn parts_from_compressed(compressed: &str) -> Option<GuidParts> {
    if compressed.len() != 22 {
        return None;
    }

    let num0 = decode_base64(&compressed[0..2])?;
    let num1 = decode_base64(&compressed[2..6])?;
    let num2 = decode_base64(&compressed[6..10])?;
    let num3 = decode_base64(&compressed[10..14])?;
    let num4 = decode_base64(&compressed[14..18])?;
    let num5 = decode_base64(&compressed[18..22])?;

    Some(GuidParts {
        data1: num0 * 16_777_216 + num1,
        data2: (num2 / 256) as u16,
        data3: (((num2 % 256) * 256) + (num3 / 65_536)) as u16,
        data4: [
            ((num3 / 256) % 256) as u8,
            (num3 % 256) as u8,
            (num4 / 65_536) as u8,
            ((num4 / 256) % 256) as u8,
            (num4 % 256) as u8,
            (num5 / 65_536) as u8,
            ((num5 / 256) % 256) as u8,
            (num5 % 256) as u8,
        ],
    })
}

fn encode_base64_fixed(mut number: u32, len: usize, out: &mut String) -> Option<()> {
    let mut chars = vec!['0'; len];
    for index in (0..len).rev() {
        chars[index] = CONVERSION_TABLE[(number % 64) as usize] as char;
        number /= 64;
    }
    if number != 0 {
        return None;
    }
    out.extend(chars);
    Some(())
}

fn decode_base64(value: &str) -> Option<u32> {
    let mut result = 0u32;
    for byte in value.bytes() {
        let index = CONVERSION_TABLE
            .iter()
            .position(|candidate| *candidate == byte)? as u32;
        result = (result * 64) + index;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_ifc_guid() {
        let expanded = expand_ifc_guid("1xS3BCk291UvhgP2a6eflL").unwrap();
        assert_eq!(expanded.len(), 36);
        assert_eq!(
            compress_uuid_string(&expanded).as_deref(),
            Some("1xS3BCk291UvhgP2a6eflL")
        );
    }

    #[test]
    fn test_guid_round_trip() {
        let compressed = "2O2Fr$t4X7Zf8NOew3FNtn";
        let expanded = expand_ifc_guid(compressed).unwrap();
        assert_eq!(compress_uuid_string(&expanded).as_deref(), Some(compressed));
    }

    #[test]
    fn test_invalid_guid_rejected() {
        assert!(expand_ifc_guid("short").is_none());
        assert!(compress_uuid_string("not-a-uuid").is_none());
    }
}

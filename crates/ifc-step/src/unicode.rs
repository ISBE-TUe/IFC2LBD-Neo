/// Decode IFC STEP Unicode escape sequences in a string.
///
/// IFC STEP uses two escape formats:
/// - `\X\HH` — single byte (ISO 8859-1 code point)
/// - `\X2\HHHH...\X0\` — multi-byte (UTF-16 code units)
/// - `\S\c` — ISO 8859-1 character with high bit set (char + 128)
pub fn decode_ifc_unicode(input: &str) -> String {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        if i + 3 < len && bytes[i] == b'\\' && bytes[i + 1] == b'X' && bytes[i + 2] == b'\\' {
            // \X\HH — single byte
            if i + 5 <= len {
                if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 3..i + 5]) {
                    if let Ok(code) = u8::from_str_radix(hex_str, 16) {
                        // ISO 8859-1 code point — map to Unicode
                        result.push(code as char);
                        i += 5;
                        continue;
                    }
                }
            }
        } else if i + 4 < len
            && bytes[i] == b'\\'
            && bytes[i + 1] == b'X'
            && bytes[i + 2] == b'2'
            && bytes[i + 3] == b'\\'
        {
            // \X2\HHHH...\X0\ — UTF-16 code units
            i += 4;
            let mut utf16_units: Vec<u16> = Vec::new();
            while i + 4 <= len {
                // Check for \X0\ terminator
                if i + 4 <= len
                    && bytes[i] == b'\\'
                    && bytes[i + 1] == b'X'
                    && bytes[i + 2] == b'0'
                    && bytes[i + 3] == b'\\'
                {
                    i += 4;
                    break;
                }
                if let Ok(hex_str) = std::str::from_utf8(&bytes[i..i + 4]) {
                    if let Ok(code) = u16::from_str_radix(hex_str, 16) {
                        utf16_units.push(code);
                        i += 4;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            // Decode UTF-16 to UTF-8
            for ch in char::decode_utf16(utf16_units.into_iter()) {
                match ch {
                    Ok(c) => result.push(c),
                    Err(_) => result.push('\u{FFFD}'),
                }
            }
            continue;
        } else if i + 3 < len && bytes[i] == b'\\' && bytes[i + 1] == b'S' && bytes[i + 2] == b'\\'
        {
            // \S\c — ISO 8859-1 with high bit set
            let c = bytes[i + 3];
            result.push((c.wrapping_add(128)) as char);
            i += 4;
            continue;
        }

        // Regular character
        if bytes[i] < 0x80 {
            result.push(bytes[i] as char);
        } else {
            // Pass through multi-byte UTF-8 sequences
            let s = &input[i..];
            if let Some(ch) = s.chars().next() {
                result.push(ch);
                i += ch.len_utf8();
                continue;
            }
        }
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_escapes() {
        assert_eq!(decode_ifc_unicode("Hello World"), "Hello World");
    }

    #[test]
    fn test_x_escape() {
        // \X\E4 = 0xE4 = 'ä' in ISO 8859-1
        assert_eq!(decode_ifc_unicode("M\\X\\E4rz"), "März");
    }

    #[test]
    fn test_x2_escape() {
        // \X2\00E4\X0\ = U+00E4 = 'ä'
        assert_eq!(decode_ifc_unicode("M\\X2\\00E4\\X0\\rz"), "März");
    }

    #[test]
    fn test_s_escape() {
        // \S\d = 'd' + 128 = 0xE4 = 'ä'
        assert_eq!(decode_ifc_unicode("M\\S\\drz"), "März");
    }

    #[test]
    fn test_plain_ascii() {
        assert_eq!(decode_ifc_unicode("IFCWALL"), "IFCWALL");
    }
}

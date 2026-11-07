//! Color parsing, formatting and channel-gain arithmetic.
use astracore_types::Rgb;
use std::io;

/// Parse six `RRGGBB` hexadecimal digits.
pub fn parse_hex(value: &str) -> io::Result<Rgb> {
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "color must be six RRGGBB hexadecimal digits",
        ));
    }
    let channel = |index: usize| u8::from_str_radix(&value[index..index + 2], 16).unwrap_or(0);
    Ok([channel(0), channel(2), channel(4)])
}

/// Format as uppercase `RRGGBB`.
pub fn hex(rgb: Rgb) -> String {
    format!("{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

/// `round(input * gain / 255 * brightness / 100)` per channel; never exceeds the input.
pub(crate) fn corrected(rgb: Rgb, gains: Rgb, brightness: u8) -> Rgb {
    std::array::from_fn(|index| {
        ((u32::from(rgb[index]) * u32::from(gains[index]) * u32::from(brightness) + 12750) / 25500)
            as u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_round_trip() {
        assert_eq!(parse_hex("80c0FF").unwrap(), [0x80, 0xc0, 0xff]);
        assert_eq!(hex([0x80, 0xc0, 0xff]), "80C0FF");
        for bad in ["", "12345", "1234567", "GG0000", "+12345"] {
            assert!(parse_hex(bad).is_err(), "{bad}");
        }
    }
}

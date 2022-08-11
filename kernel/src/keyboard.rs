pub fn scancode_to_char(code: u8) -> Option<char> {
    match code {
        0x00 => {
            panic!("Error Scancode 0x00")
        }
        0x02..=0x0a => {
            Some((b'0' + code - 1) as char)
        }
        0x0b => Some('0'),
        0x10..=0x19 => {
            Some(['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'][code as usize - 0x10])
        }
        0x1c => Some('\n'),
        0x0e => Some(0x08 as char),
        0x1e..=0x26 => {
            Some(['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'][code as usize - 0x1e])
        }
        0x2c..=0x32 => {
            Some(['z', 'x', 'c', 'v', 'b', 'n', 'm'][code as usize - 0x2c])
        }
        0x39 => Some(' '),
        0x80.. => None,
        _ => Some('?')
    }
}
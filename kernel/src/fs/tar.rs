//! Minimal ustar tar parser.
//!
//! Reads a whole tar image from memory and replays each entry into the VFS
//! via [`crate::fs`].  Supported entry types: regular files (`'0'`/`NUL`) and
//! directories (`'5'`); everything else (links, devices, pax) is skipped.
//! GNU long-name entries are not handled -- keep paths short when creating
//! the image (`tar --format=ustar`).

#![allow(dead_code)]

use alloc::vec::Vec;

use crate::fs;

const BLOCK: usize = 512;

/// Parse a NUL/space padded octal number (tar encodes sizes in octal).
fn octal(field: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &b in field {
        if b == b' ' || b == 0 {
            continue;
        }
        if !(b'0'..=b'7').contains(&b) {
            break;
        }
        v = v * 8 + (b - b'0') as u64;
    }
    v
}

fn cstr(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

/// Strip a leading `./` that `tar -C dir -cf img .` adds.
fn strip_name(name: &[u8]) -> &[u8] {
    if let Some(rest) = name.strip_prefix(b"./") {
        rest
    } else {
        name
    }
}

/// Import every entry of a tar archive into the mounted VFS.
pub fn load(buf: &[u8]) -> usize {
    let mut off = 0usize;
    let mut imported = 0usize;

    while off + BLOCK <= buf.len() {
        let header = &buf[off..off + BLOCK];

        // Two consecutive zero blocks mark the end of the archive.
        if header.iter().all(|&b| b == 0) {
            break;
        }

        let name = strip_name(cstr(&header[0..100]));
        let size = octal(&header[124..136]) as usize;
        let kind = header[156];

        let data_off = off + BLOCK;
        let data_end = data_off + size;
        let next = data_off + size.div_ceil(BLOCK) * BLOCK;
        if next > buf.len() {
            break;
        }

        let ok = match kind {
            b'5' => fs::mkdir(name),
            b'0' | 0 => {
                if name.is_empty() || size == 0 {
                    // Zero-length files still get created; empty name is junk.
                    if name.is_empty() {
                        false
                    } else {
                        fs::write_file(name, Vec::new())
                    }
                } else {
                    let data = buf[data_off..data_end].to_vec();
                    fs::write_file(name, data)
                }
            }
            _ => false, // links, devices, pax headers, ...
        };
        if ok {
            imported += 1;
        }

        off = next;
        if off >= buf.len() {
            break;
        }
    }
    imported
}

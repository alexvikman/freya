//! Ogg pages (RFC 3533), as many packets of one size a page, so the length of a whole file is
//! known before it is written.

use std::io::{self, Write};

/// The packets a page carries, but the last.
pub const PACKETS_PER_PAGE: u64 = 50;

/// What a page starts or ends.
pub const FIRST: u8 = 0x02;
pub const LAST: u8 = 0x04;

/// The bytes of a page with `sizes` packets: its header, its segment table and them.
fn page_length(sizes: impl Iterator<Item = usize>) -> u64 {
    sizes.map(|size| (lacing(size) + size) as u64).sum::<u64>() + 27
}

/// The segment table's bytes for a packet of `size`: one for each 255 bytes, and one less than
/// 255 to end it, 0 when it ends evenly.
fn lacing(size: usize) -> usize {
    size / 255 + 1
}

/// The bytes of the pages of `packets` packets of `size` each.
pub fn pages_length(packets: u64, size: usize) -> u64 {
    let full = packets / PACKETS_PER_PAGE;
    let rest = packets % PACKETS_PER_PAGE;
    let page = page_length(std::iter::repeat_n(size, PACKETS_PER_PAGE as usize));
    full * page
        + if rest > 0 {
            page_length(std::iter::repeat_n(size, rest as usize))
        } else {
            0
        }
}

/// The bytes of a page with the one packet of `size`.
pub fn single_length(size: usize) -> u64 {
    page_length(std::iter::once(size))
}

/// Writes one logical stream's pages, in turn.
pub struct Writer<W> {
    out: W,
    serial: u32,
    sequence: u32,
}

impl<W: Write> Writer<W> {
    pub fn new(out: W, serial: u32) -> Self {
        Self {
            out,
            serial,
            sequence: 0,
        }
    }

    /// Writes a page of `packets`, `flags` [`FIRST`] or [`LAST`] or neither, ending at
    /// `granule`.
    pub fn page(&mut self, packets: &[&[u8]], flags: u8, granule: u64) -> io::Result<()> {
        let mut table = Vec::new();
        for packet in packets {
            table.extend(std::iter::repeat_n(255u8, packet.len() / 255));
            table.push((packet.len() % 255) as u8);
        }
        if table.len() > 255 {
            return Err(io::Error::other("too many packets for an Ogg page"));
        }
        let mut page =
            Vec::with_capacity(27 + table.len() + packets.iter().map(|p| p.len()).sum::<usize>());
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(flags);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&self.serial.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(table.len() as u8);
        page.extend_from_slice(&table);
        for packet in packets {
            page.extend_from_slice(packet);
        }
        let crc = crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
        self.out.write_all(&page)
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

/// Ogg's CRC-32: polynomial 0x04c11db7, not reflected, from 0.
fn crc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |crc, &byte| {
        (crc << 8) ^ CRC_TABLE[usize::from((crc >> 24) as u8 ^ byte)]
    })
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04c1_1db7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_as_long_as_reckoned() {
        for (packets, size) in [
            (1, 240),
            (50, 400),
            (51, 400),
            (137, 255),
            (3, 510),
            (99, 1),
        ] {
            let data = vec![7u8; size];
            let mut writer = Writer::new(Vec::new(), 1);
            let all: Vec<&[u8]> = (0..packets).map(|_| data.as_slice()).collect();
            for (i, page) in all.chunks(PACKETS_PER_PAGE as usize).enumerate() {
                writer.page(page, 0, i as u64).unwrap();
            }
            let written = writer.into_inner().len() as u64;
            assert_eq!(written, pages_length(packets, size), "{packets} of {size}");
        }
    }

    #[test]
    fn the_crc_is_oggs() {
        // The CRC of "123456789" with Ogg's parameters (CRC-32/MPEG-2 without its start and
        // end inversions).
        assert_eq!(crc(b"123456789"), 0x89a1_897f);
    }
}

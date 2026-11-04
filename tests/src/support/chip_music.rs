//! Tiny chip-music files built by hand for the gme tests: a VGM that plays one PSG tone (with a
//! GD3 tag and a loop), the same gzipped as a VGZ, and an NSF whose init routine starts one
//! square wave. No game data.

/// Little-endian u32 at `at`.
fn put32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn utf16z(out: &mut Vec<u8>, s: &str) {
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
}

/// A VGM 1.50 file for the SN76489 PSG: one tone for `intro_s` seconds, then a loop of
/// `loop_s` seconds, with a GD3 tag naming the track `song`, the game `game` and the system
/// `system`.
pub fn vgm_psg(intro_s: u32, loop_s: u32, song: &str, game: &str, system: &str) -> Vec<u8> {
    let mut data = Vec::new();
    // Channel 0: tone period 0x0FE (about 440 Hz at 3.58 MHz), full volume.
    for b in [0x80 | 0x0E, 0x0F, 0x90] {
        data.extend_from_slice(&[0x50, b]);
    }
    let wait = |data: &mut Vec<u8>, seconds: u32| {
        let mut left = seconds * 44_100;
        while left > 0 {
            let n = left.min(0xFFFF);
            data.push(0x61);
            data.extend_from_slice(&(n as u16).to_le_bytes());
            left -= n;
        }
    };
    wait(&mut data, intro_s);
    let loop_at = 0x40 + data.len();
    wait(&mut data, loop_s);
    data.push(0x66);

    let mut gd3 = Vec::new();
    for s in [song, "", game, "", system, "", "", "", "", "", ""] {
        utf16z(&mut gd3, s);
    }

    let mut out = vec![0u8; 0x40];
    out[0..4].copy_from_slice(b"Vgm ");
    put32(&mut out, 0x08, 0x150);
    put32(&mut out, 0x0C, 3_579_545);
    put32(&mut out, 0x18, (intro_s + loop_s) * 44_100);
    if loop_s > 0 {
        put32(&mut out, 0x1C, (loop_at - 0x1C) as u32);
        put32(&mut out, 0x20, loop_s * 44_100);
    }
    put32(&mut out, 0x34, 0x40 - 0x34);
    out.extend_from_slice(&data);
    let gd3_at = out.len();
    put32(&mut out, 0x14, (gd3_at - 0x14) as u32);
    out.extend_from_slice(b"Gd3 ");
    out.extend_from_slice(&0x100u32.to_le_bytes());
    out.extend_from_slice(&(gd3.len() as u32).to_le_bytes());
    out.extend_from_slice(&gd3);
    let eof = out.len();
    put32(&mut out, 0x04, (eof - 0x04) as u32);
    out
}

/// `data` as a gzip member, deflated by zlib-ng. `None` when zlib-ng is not built.
pub fn gzip(data: &[u8]) -> Option<Vec<u8>> {
    let body = romexplorer::rom::zlib_ng::raw_deflate(data, 6)?;
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0xFF];
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    Some(out)
}

/// An NSF with `songs` songs. Init turns on pulse 1 at full constant volume; play does
/// nothing, so every song is one steady tone with no length of its own.
pub fn nsf(songs: u8, title: &str) -> Vec<u8> {
    let mut h = vec![0u8; 0x80];
    h[0..5].copy_from_slice(b"NESM\x1A");
    h[5] = 1;
    h[6] = songs;
    h[7] = 1;
    h[8..10].copy_from_slice(&0x8000u16.to_le_bytes()); // load
    h[10..12].copy_from_slice(&0x8000u16.to_le_bytes()); // init
    h[12..14].copy_from_slice(&0x8014u16.to_le_bytes()); // play
    let t = title.as_bytes();
    h[0x0E..0x0E + t.len().min(31)].copy_from_slice(&t[..t.len().min(31)]);
    h[0x6E..0x70].copy_from_slice(&16_639u16.to_le_bytes()); // NTSC play period, µs
    h[0x78..0x7A].copy_from_slice(&19_997u16.to_le_bytes());
    #[rustfmt::skip]
    let code = [
        0xA9, 0x0F, 0x8D, 0x15, 0x40, // LDA #$0F, STA $4015
        0xA9, 0xBF, 0x8D, 0x00, 0x40, // LDA #$BF, STA $4000  duty 50%, constant volume 15
        0xA9, 0xFD, 0x8D, 0x02, 0x40, // LDA #$FD, STA $4002  period low
        0xA9, 0x00, 0x8D, 0x03, 0x40, // LDA #$00, STA $4003  period high, starts the note
        0x60,                         // play: RTS   (init falls through to it)
    ];
    h.extend_from_slice(&code);
    h
}

/// A Game Gear SGC with `songs` songs. Init writes one PSG tone at full volume through port
/// 0x7F; play does nothing.
pub fn sgc(songs: u8, title: &str) -> Vec<u8> {
    let mut h = vec![0u8; 0xA0];
    h[0..4].copy_from_slice(b"SGC\x1A");
    h[4] = 1;
    h[8..10].copy_from_slice(&0x0400u16.to_le_bytes()); // load
    h[10..12].copy_from_slice(&0x0400u16.to_le_bytes()); // init
    h[12..14].copy_from_slice(&0x0410u16.to_le_bytes()); // play
    h[14..16].copy_from_slice(&0xDFF0u16.to_le_bytes()); // stack
    h[0x20..0x24].copy_from_slice(&[0, 1, 2, 3]); // mapper banks
    h[0x25] = songs;
    h[0x28] = 1; // Game Gear
    let t = title.as_bytes();
    h[0x40..0x40 + t.len().min(32)].copy_from_slice(&t[..t.len().min(32)]);
    #[rustfmt::skip]
    let mut code = vec![
        0x3E, 0x8E, 0xD3, 0x7F, // LD A,$8E / OUT ($7F),A   tone 0, period low
        0x3E, 0x0F, 0xD3, 0x7F, // LD A,$0F / OUT ($7F),A   period high
        0x3E, 0x90, 0xD3, 0x7F, // LD A,$90 / OUT ($7F),A   volume 0 = loudest
        0xC9,                   // RET
    ];
    code.resize(0x10, 0);
    code.push(0xC9); // play: RET
    h.extend_from_slice(&code);
    h
}

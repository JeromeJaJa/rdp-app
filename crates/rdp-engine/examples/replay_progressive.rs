//! Replays a RemoteFX Progressive capture (see `capture_progressive` in gfx/client.rs)
//! through the decoder and reports which messages fail.

use ironrdp_graphics::progressive::ProgressiveDecoder;

fn main() {
    let path = std::env::args().nth(1).expect("usage: replay_progressive <capture file>");
    let data = std::fs::read(&path).expect("read capture");
    let mut decoder = ProgressiveDecoder::new();
    let (mut pos, mut index, mut ok, mut failed) = (0usize, 0usize, 0usize, 0usize);
    let mut errors = std::collections::BTreeMap::<String, usize>::new();
    while pos + 14 <= data.len() {
        let surface = u16::from_le_bytes([data[pos], data[pos + 1]]);
        let context = u32::from_le_bytes(data[pos + 2..pos + 6].try_into().unwrap());
        let w = u16::from_le_bytes([data[pos + 6], data[pos + 7]]);
        let h = u16::from_le_bytes([data[pos + 8], data[pos + 9]]);
        let len = u32::from_le_bytes(data[pos + 10..pos + 14].try_into().unwrap()) as usize;
        pos += 14;
        let bitmap = &data[pos..pos + len];
        pos += len;
        decoder.begin_frame();
        match decoder.decode_bitmap(surface, context, w, h, bitmap) {
            Ok(tiles) => {
                ok += 1;
                let _ = tiles;
            }
            Err(e) => {
                failed += 1;
                *errors.entry(format!("{e:?}")).or_default() += 1;
                if failed <= 5 {
                    eprintln!("message {index} ({len} bytes, surface {surface} ctx {context} {w}x{h}): {e:?}");
                }
            }
        }
        decoder.end_frame();
        index += 1;
    }
    println!("{index} messages: {ok} ok, {failed} failed {errors:?}");
}

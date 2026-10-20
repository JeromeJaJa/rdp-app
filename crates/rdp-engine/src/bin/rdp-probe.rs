//! Headless engine test harness.
//!
//! Connects one or more sessions from a hosts file (`host[:port]:user:pass` per
//! line, never printed), keeps them visible for a while, prints events and
//! frame statistics, and writes a PNG of the first session's framebuffer.
//!
//! ```text
//! rdp-probe [--file test-hosts.txt] [--line 1] [--count 1] [--seconds 15]
//!           [--codec auto|h264|remotefx] [--size 1600x900] [--out probe.png]
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use parking_lot::Mutex;
use rdp_engine::surface::{PIXEL_OFFSET, surface_len};
use rdp_engine::{
    CodecPreference, ConnectParams, Engine, EngineEvent, EngineSettings, HeapSurface, SurfaceKind, SurfaceMemory,
    SurfaceProvider, Viewport,
};

/// A surface with its width and height.
type SurfaceEntry = (Arc<HeapSurface>, u32, u32);

#[derive(Default)]
struct ProbeProvider {
    surfaces: Mutex<HashMap<(String, SurfaceKind), SurfaceEntry>>,
}

impl SurfaceProvider for ProbeProvider {
    fn allocate(&self, session: &str, kind: SurfaceKind, w: u32, h: u32) -> anyhow::Result<Arc<dyn SurfaceMemory>> {
        let mem = Arc::new(HeapSurface::new(surface_len(w, h)));
        self.surfaces
            .lock()
            .insert((session.to_owned(), kind), (Arc::clone(&mem), w, h));
        Ok(mem)
    }

    fn release(&self, session: &str, kind: SurfaceKind) {
        self.surfaces.lock().remove(&(session.to_owned(), kind));
    }
}

struct Args {
    file: String,
    line: usize,
    count: usize,
    seconds: u64,
    codec: CodecPreference,
    size: (u16, u16),
    out: String,
    avc444: bool,
    decoder: rdp_engine::DecoderPreference,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut a = Args {
        file: "test-hosts.txt".into(),
        line: 1,
        count: 1,
        seconds: 15,
        codec: CodecPreference::Auto,
        size: (1600, 900),
        out: "probe.png".into(),
        avc444: true,
        decoder: rdp_engine::DecoderPreference::Auto,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().context("missing value");
        match flag.as_str() {
            "--file" => a.file = val()?,
            "--line" => a.line = val()?.parse()?,
            "--count" => a.count = val()?.parse()?,
            "--seconds" => a.seconds = val()?.parse()?,
            "--codec" => {
                a.codec = match val()?.as_str() {
                    "auto" => CodecPreference::Auto,
                    "h264" => CodecPreference::H264,
                    "remotefx" => CodecPreference::RemoteFx,
                    other => bail!("unknown codec {other}"),
                }
            }
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').context("size must be WxH")?;
                a.size = (w.parse()?, h.parse()?);
            }
            "--out" => a.out = val()?,
            "--no-444" => a.avc444 = false,
            "--decoder" => {
                a.decoder = match val()?.as_str() {
                    "auto" => rdp_engine::DecoderPreference::Auto,
                    "hardware" => rdp_engine::DecoderPreference::Hardware,
                    "software" => rdp_engine::DecoderPreference::Software,
                    other => bail!("unknown decoder {other}"),
                }
            }
            other => bail!("unknown flag {other}"),
        }
    }
    Ok(a)
}

fn parse_host_line(line: &str) -> Option<(String, u16, String, String)> {
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    match parts.as_slice() {
        [host, port, user, pass] if port.parse::<u16>().is_ok() => {
            Some((host.to_string(), port.parse().ok()?, user.to_string(), pass.to_string()))
        }
        _ => {
            let (host, rest) = line.split_once(':')?;
            let (user, pass) = rest.split_once(':')?;
            Some((host.to_owned(), 3389, user.to_owned(), pass.to_owned()))
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RDP_LOG").unwrap_or_else(|_| "warn,rdp_engine=info".into()),
        )
        .compact()
        .init();

    let args = parse_args()?;
    let text = std::fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let hosts: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(parse_host_line)
        .collect();
    anyhow::ensure!(args.line >= 1 && args.line <= hosts.len(), "no such line in hosts file");
    // Session i uses host line (line + i), wrapping, so a multi-port file spreads the load.
    let host_for = |i: usize| hosts[(args.line - 1 + i) % hosts.len()].clone();
    let (host, port, user, _) = host_for(0);

    let provider = Arc::new(ProbeProvider::default());
    let connected = Arc::new(AtomicU32::new(0));
    let finished = Arc::new(AtomicU32::new(0));
    let t0 = Instant::now();
    let events = {
        let connected = Arc::clone(&connected);
        let finished = Arc::clone(&finished);
        Arc::new(move |e: EngineEvent| {
            let ms = t0.elapsed().as_millis();
            match &e {
                EngineEvent::Cursor { id, cursor } => {
                    println!(
                        "[{ms:>6}ms] {id} cursor {}",
                        if cursor.len() > 40 { "<image>" } else { cursor }
                    );
                }
                EngineEvent::Status { id, status, message } => {
                    println!(
                        "[{ms:>6}ms] {id} status {status:?} {}",
                        message.as_deref().unwrap_or("")
                    );
                    match status {
                        rdp_engine::SessionStatus::Connected => {
                            connected.fetch_add(1, Ordering::Relaxed);
                        }
                        rdp_engine::SessionStatus::Idle | rdp_engine::SessionStatus::Error => {
                            finished.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
                other => println!("[{ms:>6}ms] {other:?}"),
            }
        })
    };

    let engine = Engine::new(
        provider.clone(),
        events,
        EngineSettings {
            max_concurrent_connects: 8,
            ..Default::default()
        },
    );
    engine.set_viewport(Viewport {
        width: args.size.0,
        height: args.size.1,
        scale: 100,
    });

    let ids: Vec<String> = (0..args.count).map(|i| format!("s{i}")).collect();
    engine.set_visibility(vec![ids[0].clone()], ids.iter().skip(1).cloned().collect());
    println!("connecting {} session(s) to {host}:{port} as {user}", args.count);
    for (i, id) in ids.iter().enumerate() {
        let (host, port, user, pass) = host_for(i);
        engine.connect(
            id,
            ConnectParams {
                codec: args.codec,
                h264_444: args.avc444,
                decoder: args.decoder,
                auto_reconnect: false,
                ..ConnectParams::new(host, port, user, pass)
            },
        );
    }

    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut last_seq = 0u32;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(1));
        if finished.load(Ordering::Relaxed) as usize >= args.count {
            break;
        }
        let surfaces = provider.surfaces.lock();
        if let Some((mem, w, h)) = surfaces.get(&(ids[0].clone(), SurfaceKind::Full)) {
            let seq = u32::from_le_bytes(mem.bytes()[20..24].try_into().unwrap());
            println!(
                "[{:>6}ms] {} full {w}x{h}: {} rect updates/s, {} connected",
                t0.elapsed().as_millis(),
                ids[0],
                seq.wrapping_sub(last_seq),
                connected.load(Ordering::Relaxed)
            );
            last_seq = seq;
        }
    }

    if let Some((mem, w, h)) = provider
        .surfaces
        .lock()
        .get(&(ids[0].clone(), SurfaceKind::Full))
        .cloned()
    {
        let pixels = &mem.bytes()[PIXEL_OFFSET..PIXEL_OFFSET + (w * h * 4) as usize];
        let file = std::fs::File::create(&args.out)?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        let mut wr = enc.write_header()?;
        let opaque: Vec<u8> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect();
        wr.write_image_data(&opaque)?;
        println!("wrote {} ({w}x{h})", args.out);
    }

    engine.disconnect_all();
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

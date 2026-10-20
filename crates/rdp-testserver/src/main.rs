//! Local RDP test servers (IronRDP server, CredSSP + TLS, RemoteFX/bitmap output).
//!
//! Each port gets an independent server with animated content: a sweeping bar
//! and a marker that follows the client mouse (its colour changes with key
//! presses and clicks), so rendering, input and resizing can be verified by eye.
//!
//! With `--h264` the content is streamed as H.264 (AVC420) over the graphics
//! pipeline instead, like a modern Windows host.
//!
//! ```text
//! rdp-testserver [--base-port 33890] [--count 1] [--user test] [--pass test] [--h264] [--mix] [--fps 30]
//!
//! `--mix` (with `--h264`) exercises several codecs per frame the way Windows
//! does: H.264 for the moving bar (partial region), ClearCodec for the mouse
//! marker and a swatch, Planar / uncompressed for a swatch at the origin.
//! ```

mod avc444;
mod gfx;

use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat, RdpServer,
    RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerResult, TlsIdentityCtx,
};

#[derive(Default)]
struct Shared {
    mouse: (u16, u16),
    keys: u32,
    clicks: u32,
    pending_resize: Option<DesktopSize>,
}

#[derive(Clone)]
struct Handler {
    port: u16,
    shared: Arc<Mutex<Shared>>,
    size: Arc<Mutex<DesktopSize>>,
    h264: bool,
}

/// Display updates when frames go over the graphics pipeline instead.
struct Idle;

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Idle {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        std::future::pending().await
    }
}

impl RdpServerInputHandler for Handler {
    fn keyboard(&mut self, event: KeyboardEvent) {
        if let KeyboardEvent::Pressed { .. } | KeyboardEvent::UnicodePressed(_) = event {
            self.shared.lock().unwrap().keys += 1;
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        let mut s = self.shared.lock().unwrap();
        match event {
            MouseEvent::Move { x, y } => s.mouse = (x, y),
            MouseEvent::Button { x, y, pressed, .. } => {
                s.mouse = (x, y);
                if pressed {
                    s.clicks += 1;
                }
            }
            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for Handler {
    async fn size(&mut self) -> DesktopSize {
        *self.size.lock().unwrap()
    }

    async fn request_initial_size(&mut self, client: DesktopSize) -> DesktopSize {
        let size = DesktopSize {
            width: client.width.clamp(200, 3840),
            height: client.height.clamp(200, 2160),
        };
        *self.size.lock().unwrap() = size;
        size
    }

    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        if self.h264 {
            return Ok(Box::new(Idle));
        }
        let size = *self.size.lock().unwrap();
        Ok(Box::new(Animator::new(self.port, size, Arc::clone(&self.shared))))
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        if self.h264 {
            return; // fixed-size surface in this mode
        }
        if let Some(m) = layout.monitors().first() {
            let (w, h) = m.dimensions();
            let size = DesktopSize {
                width: (w as u16).clamp(200, 3840),
                height: (h as u16).clamp(200, 2160),
            };
            *self.size.lock().unwrap() = size;
            self.shared.lock().unwrap().pending_resize = Some(size);
        }
    }
}

struct Animator {
    port: u16,
    w: u16,
    h: u16,
    fb: Vec<u8>, // BGRA
    shared: Arc<Mutex<Shared>>,
    started: Instant,
    frame: u64,
    first: bool,
    bar_x: u16,
    marker: (u16, u16),
    /// Draw a block of single-pixel red/blue checkerboard (survives only 4:4:4 chroma).
    chroma_detail: bool,
}

const BAR_W: u16 = 64;
const MARK: u16 = 24;

/// x, y, width, height.
type Rect = (u16, u16, u16, u16);
impl Animator {
    fn new(port: u16, size: DesktopSize, shared: Arc<Mutex<Shared>>) -> Self {
        let mut a = Self {
            port,
            w: size.width,
            h: size.height,
            fb: Vec::new(),
            shared,
            started: Instant::now(),
            frame: 0,
            first: true,
            bar_x: 0,
            marker: (0, 0),
            chroma_detail: false,
        };
        a.reset(size);
        a
    }

    fn enable_chroma_detail(&mut self) {
        self.chroma_detail = true;
        self.reset(DesktopSize {
            width: self.w,
            height: self.h,
        });
    }

    /// Where the chroma detail block is drawn (x, y, w, h).
    fn chroma_detail_rect(&self) -> (u16, u16, u16, u16) {
        (self.w / 6, self.h / 6 + self.h / 2 + 20, 160, 80)
    }

    fn hue(&self) -> u32 {
        (u32::from(self.port) * 47) % 360
    }

    fn reset(&mut self, size: DesktopSize) {
        self.w = size.width;
        self.h = size.height;
        self.fb = vec![0; usize::from(self.w) * usize::from(self.h) * 4];
        self.restore_background(0, 0, self.w, self.h);
        // A static "window" with text-like lines.
        let (wx, wy, ww, wh) = (self.w / 6, self.h / 6, self.w / 2, self.h / 2);
        self.fill(wx, wy, ww, wh, 245, 245, 247);
        self.fill(wx, wy, ww, 28, 225, 226, 230);
        for line in 0..(wh / 22).saturating_sub(2) {
            let len = (ww - 40) * (3 + (line * 7) % 5) / 8;
            self.fill(wx + 20, wy + 48 + line * 22, len, 8, 120, 122, 130);
        }
        if self.chroma_detail {
            let (x, y, w, h) = self.chroma_detail_rect();
            for yy in y..(y + h).min(self.h) {
                for xx in x..(x + w).min(self.w) {
                    if (xx + yy) % 2 == 0 {
                        self.put(xx, yy, 230, 30, 40);
                    } else {
                        self.put(xx, yy, 30, 60, 230);
                    }
                }
            }
        }
        self.first = true;
        self.marker = (0, 0);
        self.bar_x = 0;
    }

    fn put(&mut self, x: u16, y: u16, r: u8, g: u8, b: u8) {
        let i = (usize::from(y) * usize::from(self.w) + usize::from(x)) * 4;
        self.fb[i..i + 4].copy_from_slice(&[b, g, r, 255]);
    }

    #[allow(clippy::too_many_arguments)] // a rectangle and a colour
    fn fill(&mut self, x: u16, y: u16, w: u16, h: u16, r: u8, g: u8, b: u8) {
        for yy in y..(y.saturating_add(h)).min(self.h) {
            for xx in x..(x.saturating_add(w)).min(self.w) {
                self.put(xx, yy, r, g, b);
            }
        }
    }

    fn restore_background(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let hue = self.hue();
        for yy in y..(y.saturating_add(h)).min(self.h) {
            for xx in x..(x.saturating_add(w)).min(self.w) {
                let (r, g, b) = background(hue, xx, yy, self.w, self.h);
                self.put(xx, yy, r, g, b);
            }
        }
    }

    fn width(&self) -> u16 {
        self.w
    }

    fn height(&self) -> u16 {
        self.h
    }

    /// Advances the whole animation one frame (graphics pipeline mode) and returns
    /// the changed bar and marker rectangles (x, y, w, h).
    fn step(&mut self, t: f32) -> (Rect, Rect) {
        (self.move_bar(t), self.move_marker())
    }

    /// Paints a solid swatch and returns its rectangle.
    fn swatch(&mut self, x: u16, y: u16, size: u16, rgb: (u8, u8, u8)) -> (u16, u16, u16, u16) {
        self.fill(x, y, size, size, rgb.0, rgb.1, rgb.2);
        (x, y, size.min(self.w - x), size.min(self.h - y))
    }

    /// A rectangle of the framebuffer as tightly packed BGRA.
    fn bgra_region(&self, (x, y, w, h): (u16, u16, u16, u16)) -> Vec<u8> {
        let mut out = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
        for row in y..y + h {
            let start = (usize::from(row) * usize::from(self.w) + usize::from(x)) * 4;
            out.extend_from_slice(&self.fb[start..start + usize::from(w) * 4]);
        }
        out
    }

    /// The framebuffer as tightly packed RGBA.
    fn rgba(&self) -> Vec<u8> {
        self.fb
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect()
    }

    fn move_bar(&mut self, t: f32) -> (u16, u16, u16, u16) {
        let old = self.bar_x;
        let span = self.w.saturating_sub(BAR_W).max(1);
        self.bar_x = ((t * 240.0) as u32 % u32::from(span)) as u16;
        let bar_y = self.h - self.h / 5;
        let bar_h = self.h / 8;
        self.restore_background(old, bar_y, BAR_W, bar_h);
        let (r, g, b) = hsl((t * 40.0) % 360.0, 0.8, 0.6);
        self.fill(self.bar_x, bar_y, BAR_W, bar_h, r, g, b);
        let (x0, x1) = (
            old.min(self.bar_x).min(self.w),
            (old.max(self.bar_x) + BAR_W).min(self.w),
        );
        (x0, bar_y, x1.saturating_sub(x0), bar_h)
    }

    fn move_marker(&mut self) -> (u16, u16, u16, u16) {
        let (mx, my, keys, clicks) = {
            let s = self.shared.lock().unwrap();
            (s.mouse.0, s.mouse.1, s.keys, s.clicks)
        };
        let old = self.marker;
        let (nx, ny) = (mx.saturating_sub(MARK / 2), my.saturating_sub(MARK / 2));
        self.marker = (nx, ny);
        self.restore_background(old.0, old.1, MARK, MARK);
        let (r, g, b) = hsl(((keys * 30 + clicks * 90) % 360) as f32, 0.9, 0.6);
        self.fill(nx, ny, MARK, MARK, r, g, b);
        // Positions may predate a resize: clamp everything to the current size.
        let x0 = old.0.min(nx).min(self.w);
        let y0 = old.1.min(ny).min(self.h);
        let x1 = (old.0.max(nx).saturating_add(MARK)).min(self.w);
        let y1 = (old.1.max(ny).saturating_add(MARK)).min(self.h);
        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    fn region(&self, x: u16, y: u16, w: u16, h: u16) -> Option<DisplayUpdate> {
        let x = x.min(self.w - 1);
        let y = y.min(self.h - 1);
        let w = w.min(self.w - x);
        let h = h.min(self.h - y);
        let mut data = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
        for row in y..y + h {
            let start = (usize::from(row) * usize::from(self.w) + usize::from(x)) * 4;
            data.extend_from_slice(&self.fb[start..start + usize::from(w) * 4]);
        }
        Some(DisplayUpdate::Bitmap(BitmapUpdate {
            x,
            y,
            width: NonZeroU16::new(w)?,
            height: NonZeroU16::new(h)?,
            format: PixelFormat::BgrA32,
            data: data.into(),
            stride: NonZeroUsize::new(usize::from(w) * 4)?,
        }))
    }
}

fn background(hue: u32, x: u16, y: u16, w: u16, h: u16) -> (u8, u8, u8) {
    let t = f32::from(x) / f32::from(w.max(1)) * 0.6 + f32::from(y) / f32::from(h.max(1)) * 0.4;
    hsl(hue as f32, 0.45, 0.22 + t * 0.18)
}

fn hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Animator {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        let resize = self.shared.lock().unwrap().pending_resize.take();
        if let Some(size) = resize {
            self.reset(size);
            return Ok(Some(DisplayUpdate::Resize(size)));
        }
        if self.first {
            self.first = false;
            return Ok(self.region(0, 0, self.w, self.h));
        }

        tokio::time::sleep(Duration::from_millis(33)).await;
        self.frame += 1;

        // Alternate between the sweeping bar and the mouse marker.
        let t = self.started.elapsed().as_secs_f32();
        let (x, y, w, h) = if self.frame.is_multiple_of(2) {
            self.move_bar(t)
        } else {
            self.move_marker()
        };
        Ok(self.region(x, y, w, h))
    }
}

fn identity(dir: &std::path::Path) -> anyhow::Result<TlsIdentityCtx> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
    std::fs::create_dir_all(dir)?;
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem())?;
    std::fs::write(&key_path, cert.key_pair.serialize_pem())?;
    TlsIdentityCtx::init_from_paths(&cert_path, &key_path).context("TLS identity")
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("RDP_LOG").unwrap_or_else(|_| "warn".into()))
        .compact()
        .init();
    // Both rustls providers are in the tree, so pick one (ring) before any TLS work.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut base_port = 33890u16;
    let mut count = 1u16;
    let mut user = "test".to_owned();
    let mut pass = "test".to_owned();
    let mut h264 = false;
    let mut mix = false;
    let mut avc444 = None;
    let mut fps = 30u32;
    let mut drop_every = 0u32;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--h264" {
            h264 = true;
            continue;
        }
        if flag == "--avc444" || flag == "--avc444v2" {
            h264 = true;
            avc444 = Some(if flag == "--avc444" {
                avc444::Layout::V1
            } else {
                avc444::Layout::V2
            });
            continue;
        }
        if flag == "--mix" {
            h264 = true;
            mix = true;
            continue;
        }
        let val = args.next().context("missing value")?;
        match flag.as_str() {
            "--fps" => fps = val.parse()?,
            "--drop-every" => drop_every = val.parse()?,
            "--base-port" => base_port = val.parse()?,
            "--count" => count = val.parse()?,
            "--user" => user = val,
            "--pass" => pass = val,
            other => anyhow::bail!("unknown flag {other}"),
        }
    }

    let identity = identity(&std::env::temp_dir().join("rdp-testserver"))?;

    let mut tasks = Vec::new();
    for i in 0..count {
        let port = base_port + i;
        let acceptor = identity.make_acceptor()?;
        let pub_key = identity.pub_key.clone();
        let (user, pass) = (user.clone(), pass.clone());
        let handler = Handler {
            port,
            shared: Arc::default(),
            size: Arc::new(Mutex::new(DesktopSize {
                width: 1920,
                height: 1080,
            })),
            h264,
        };
        let slots = gfx::GfxSlots::default();
        let producer = h264.then(|| {
            gfx::produce(
                port,
                slots.clone(),
                Arc::clone(&handler.size),
                Arc::clone(&handler.shared),
                fps,
                drop_every,
                mix,
                avc444,
            )
        });
        // The server future is not Send: give each one its own thread + runtime.
        tasks.push(std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let mut server = RdpServer::builder()
                .with_addr(([127, 0, 0, 1], port))
                .with_hybrid(acceptor, pub_key)
                .with_input_handler(handler.clone())
                .with_display_handler(handler)
                .with_honor_client_desktop_size(Some(DesktopSize {
                    width: 3840,
                    height: 2160,
                }))
                .with_gfx_factory(
                    h264.then(|| Box::new(gfx::Factory { slots }) as Box<dyn ironrdp_server::GfxServerFactory>),
                )
                .build();
            if let Some(producer) = producer {
                rt.spawn(producer);
            }
            server.set_credentials(Some(Credentials {
                username: user.clone(),
                password: pass.clone(),
                domain: None,
            }));
            rt.block_on(async move {
                loop {
                    if let Err(e) = server.run().await {
                        tracing::warn!(port, error = %e, "server stopped; restarting");
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            });
        }));
    }
    println!(
        "{count} test server(s) on 127.0.0.1:{base_port}..{} (user {user})",
        base_port + count - 1
    );
    for t in tasks {
        let _ = t.join();
    }
    Ok(())
}

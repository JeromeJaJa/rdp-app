//! GPU H.264 decoding: the Windows Media Foundation H.264 decoder with a Direct3D 11
//! device, so decoding runs on the video engine (DXVA). Pictures are read back as
//! NV12 and colour-converted per changed region like the software path.
//!
//! One D3D11 device is shared by every decoder in the process.

use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context as _, bail};
use ironrdp_egfx::pdu::{avc_to_annex_b_into, is_avc_format};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice, ID3D11Device,
    ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::Interface as _;

use super::h264::{Chroma, Decoder, Yuv};

/// With `DecoderPreference::Auto`, sessions beyond this many use software decoding:
/// every GPU decoder holds its own pool of video surfaces.
pub(crate) const AUTO_LIMIT: usize = 8;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn active_decoders() -> usize {
    ACTIVE.load(Ordering::Relaxed)
}

struct Gpu {
    manager: IMFDXGIDeviceManager,
    _device: ID3D11Device,
}

// SAFETY: the device is multithread-protected and the DXGI device manager is a
// free-threaded object meant to be shared between decoders.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

fn gpu() -> anyhow::Result<&'static Gpu> {
    static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
    GPU.get_or_init(|| create_gpu().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn create_gpu() -> anyhow::Result<Gpu> {
    // SAFETY: plain COM / D3D11 / Media Foundation calls with valid arguments.
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("MFStartup")?;
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[
                D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
                D3D_FEATURE_LEVEL_10_0,
            ]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .context("create Direct3D 11 video device")?;
        let device = device.context("no Direct3D 11 device")?;
        // Returns the previous setting.
        let _ = device
            .cast::<ID3D11Multithread>()
            .context("ID3D11Multithread")?
            .SetMultithreadProtected(true);

        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).context("MFCreateDXGIDeviceManager")?;
        let manager = manager.context("no DXGI device manager")?;
        manager
            .ResetDevice(&device, token)
            .context("IMFDXGIDeviceManager::ResetDevice")?;
        Ok(Gpu {
            manager,
            _device: device,
        })
    }
}

pub(crate) struct MfDecoder {
    transform: IMFTransform,
    /// Allocated picture size (macroblock-aligned).
    frame: (usize, usize),
    /// Visible size (display aperture).
    visible: (usize, usize),
    provides_samples: bool,
    out_size: u32,
    time: i64,
    annex_b: Vec<u8>,
    /// The last picture, copied out of the decoder's (GPU) buffer.
    y: Vec<u8>,
    uv: Vec<u8>,
    stride: usize,
}

// SAFETY: the transform lives in the multithreaded apartment and is only used by the
// session thread that owns this decoder.
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    pub(crate) fn new() -> anyhow::Result<Self> {
        // SAFETY: COM / Media Foundation calls with valid arguments; the D3D manager
        // outlives the transform (it is process-wide).
        unsafe {
            // S_FALSE (already initialised) and RPC_E_CHANGED_MODE are both fine here.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let gpu = gpu()?;

            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).context("create H.264 MFT")?;
            let attrs = transform.GetAttributes().context("MFT attributes")?;
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                bail!("the H.264 decoder is not Direct3D 11 aware");
            }
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)
                .context("attach Direct3D device")?;
            // One picture out per access unit, no reordering delay.
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0).context("H.264 input type")?;

            let mut decoder = Self {
                transform,
                frame: (0, 0),
                visible: (0, 0),
                provides_samples: true,
                out_size: 0,
                time: 0,
                annex_b: Vec::new(),
                y: Vec::new(),
                uv: Vec::new(),
                stride: 0,
            };
            decoder.negotiate_output()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            ACTIVE.fetch_add(1, Ordering::Relaxed);
            Ok(decoder)
        }
    }

    /// Picks NV12 output and records the picture geometry.
    unsafe fn negotiate_output(&mut self) -> anyhow::Result<()> {
        unsafe {
            let mut index = 0;
            let ty = loop {
                let ty = self
                    .transform
                    .GetOutputAvailableType(0, index)
                    .context("the decoder offers no NV12 output")?;
                if ty.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    break ty;
                }
                index += 1;
            };
            self.transform.SetOutputType(0, &ty, 0).context("NV12 output type")?;

            let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            self.frame = ((size >> 32) as usize, (size & 0xFFFF_FFFF) as usize);
            let mut area = MFVideoArea::default();
            let bytes =
                std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), std::mem::size_of::<MFVideoArea>());
            self.visible = match ty.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) {
                Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => (area.Area.cx as usize, area.Area.cy as usize),
                _ => self.frame,
            };

            let info = self.transform.GetOutputStreamInfo(0)?;
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            self.provides_samples = info.dwFlags & provides != 0;
            self.out_size = info.cbSize;
            Ok(())
        }
    }

    /// Fetches one decoded picture if the decoder has one.
    unsafe fn pull(&mut self) -> anyhow::Result<bool> {
        unsafe {
            for _ in 0..4 {
                let target = if self.provides_samples {
                    None
                } else {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(self.out_size.max(1))?)?;
                    Some(sample)
                };
                let mut out = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(target),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut out, &mut status);
                let sample = ManuallyDrop::take(&mut out[0].pSample);
                drop(ManuallyDrop::take(&mut out[0].pEvents));
                match result {
                    Ok(()) => {
                        self.read_picture(&sample.context("the decoder returned no picture")?)?;
                        return Ok(true);
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.negotiate_output()?,
                    Err(e) => return Err(e).context("ProcessOutput"),
                }
            }
            bail!("the H.264 decoder keeps changing its output format")
        }
    }

    /// Copies an NV12 picture out of a decoder sample.
    unsafe fn read_picture(&mut self, sample: &IMFSample) -> anyhow::Result<()> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (w, h) = self.frame;
            // The chroma plane follows the luma plane of the *texture*, which may be
            // taller than the picture.
            let rows = texture_height(&buffer).unwrap_or(h).max(h);
            let mut data = std::ptr::null_mut();
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
                let mut pitch = 0i32;
                b2.Lock2D(&mut data, &mut pitch).context("lock picture")?;
                let result = self.store(data, pitch, rows, None);
                let _ = b2.Unlock2D();
                result
            } else {
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len)).context("lock picture")?;
                let result = self.store(data, w as i32, rows, Some(len as usize));
                let _ = buffer.Unlock();
                result
            }
        }
    }

    unsafe fn store(&mut self, data: *mut u8, pitch: i32, rows: usize, len: Option<usize>) -> anyhow::Result<()> {
        let (w, h) = self.frame;
        if data.is_null() || pitch <= 0 || (pitch as usize) < w || w == 0 || h == 0 {
            bail!("unexpected picture layout (pitch {pitch}, {w}×{h})");
        }
        let pitch = pitch as usize;
        let total = pitch * rows + pitch * h.div_ceil(2);
        if len.is_some_and(|len| len < total) {
            bail!("picture buffer too small");
        }
        // SAFETY: the locked buffer holds `rows` luma rows followed by the chroma rows,
        // each `pitch` bytes long, as checked above for plain buffers and guaranteed by
        // Media Foundation for 2D buffers.
        let src = unsafe { std::slice::from_raw_parts(data, total) };
        self.stride = w;
        self.y.resize(w * h, 0);
        self.uv.resize(w * h.div_ceil(2), 0);
        for (row, dst) in self.y.chunks_exact_mut(w).enumerate() {
            dst.copy_from_slice(&src[row * pitch..row * pitch + w]);
        }
        let chroma = &src[pitch * rows..];
        for (row, dst) in self.uv.chunks_exact_mut(w).enumerate() {
            dst.copy_from_slice(&chroma[row * pitch..row * pitch + w]);
        }
        Ok(())
    }
}

/// Height of the D3D11 texture behind a GPU buffer.
unsafe fn texture_height(buffer: &IMFMediaBuffer) -> Option<usize> {
    unsafe {
        let dxgi: IMFDXGIBuffer = buffer.cast().ok()?;
        let mut raw = std::ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw).ok()?;
        let texture = ID3D11Texture2D::from_raw(raw);
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        Some(desc.Height as usize)
    }
}

impl Decoder for MfDecoder {
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<Yuv<'_>>> {
        let input = if is_avc_format(data) {
            avc_to_annex_b_into(data, &mut self.annex_b);
            &self.annex_b[..]
        } else {
            data
        };
        // SAFETY: Media Foundation calls with valid arguments on the owning thread.
        let produced = unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(input.len() as u32)?;
            let mut dst = std::ptr::null_mut();
            buffer.Lock(&mut dst, None, None)?;
            std::ptr::copy_nonoverlapping(input.as_ptr(), dst, input.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(input.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            sample.SetSampleDuration(1)?;
            self.time += 1;

            let mut produced = false;
            loop {
                match self.transform.ProcessInput(0, &sample, 0) {
                    Ok(()) => break,
                    // Full: drain a picture, then retry.
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        if !self.pull()? {
                            bail!("the H.264 decoder accepts no input");
                        }
                        produced = true;
                    }
                    Err(e) => return Err(e).context("ProcessInput"),
                }
            }
            while self.pull()? {
                produced = true;
            }
            produced
        };
        if !produced || self.y.is_empty() {
            return Ok(None);
        }
        let (w, h) = self.visible;
        Ok(Some(Yuv {
            y: &self.y,
            y_stride: self.stride,
            chroma: Chroma::Interleaved {
                uv: &self.uv,
                stride: self.stride,
            },
            width: w.min(self.frame.0),
            height: h.min(self.frame.1),
        }))
    }

    fn name(&self) -> &'static str {
        "Media Foundation (GPU)"
    }

    fn hardware(&self) -> bool {
        true
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(all(test, feature = "openh264"))]
mod tests {
    use openh264::OpenH264API;
    use openh264::encoder::{Encoder, EncoderConfig, QpRange, UsageType};
    use openh264::formats::YUVSlices;

    use super::*;
    use crate::gfx::h264::{self, Plane};

    fn planes(p: &Yuv<'_>) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let grab = |pl: Plane<'_>, w: usize, h: usize| {
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| pl.at(x, y).unwrap())
                .collect::<Vec<_>>()
        };
        (
            grab(p.y_plane(), p.width, p.height),
            grab(p.u_plane(), p.width / 2, p.height / 2),
            grab(p.v_plane(), p.width / 2, p.height / 2),
        )
    }

    fn max_diff(a: &(Vec<u8>, Vec<u8>, Vec<u8>), b: &(Vec<u8>, Vec<u8>, Vec<u8>)) -> u8 {
        let d = |x: &[u8], y: &[u8]| x.iter().zip(y).map(|(p, q)| p.abs_diff(*q)).max().unwrap_or(0);
        d(&a.0, &b.0).max(d(&a.1, &b.1)).max(d(&a.2, &b.2))
    }

    /// The GPU decodes the same pictures as the software decoder (H.264 decoding is
    /// bit-exact). Skipped on machines without a usable GPU decoder.
    #[test]
    fn gpu_matches_software_decoder() {
        let mut gpu = match MfDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skipping: no GPU decoder ({e:#})");
                return;
            }
        };
        let mut software = h264::new_software().unwrap();
        // A size that is not macroblock-aligned exercises cropping and the aperture.
        let (w, h) = (200usize, 120usize);
        let config = EncoderConfig::new()
            .qp(QpRange::new(10, 16))
            .usage_type(UsageType::ScreenContentRealTime)
            .skip_frames(false);
        let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config).unwrap();
        for frame in 0..12usize {
            let y: Vec<u8> = (0..w * h)
                .map(|i| ((i % w + i / w * 2 + frame * 9) % 220 + 16) as u8)
                .collect();
            let u: Vec<u8> = (0..(w / 2) * (h / 2))
                .map(|i| (64 + (i % (w / 2) + frame) % 128) as u8)
                .collect();
            let v: Vec<u8> = vec![160; (w / 2) * (h / 2)];
            let bits = encoder
                .encode(&YUVSlices::new((&y, &u, &v), (w, h), (w, w / 2, w / 2)))
                .unwrap()
                .to_vec();
            let sw = planes(&software.decode(&bits).unwrap().expect("software picture"));
            let pic = gpu
                .decode(&bits)
                .unwrap()
                .expect("a GPU picture per access unit (low latency)");
            assert_eq!((pic.width, pic.height), (w, h));
            let hw = planes(&pic);
            let src = (y, u, v);
            eprintln!(
                "frame {frame}: gpu-vs-software {}, software-vs-source {}",
                max_diff(&hw, &sw),
                max_diff(&sw, &src)
            );
            assert!(
                max_diff(&hw, &sw) <= 1,
                "frame {frame}: GPU and software pictures differ"
            );
        }
        assert!(gpu.hardware());
    }
}

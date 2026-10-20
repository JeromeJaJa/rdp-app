//! Window material (Settings → Appearance), experimental: solid, or the system acrylic
//! backdrop (a blur of whatever is behind the window) showing through the page's
//! translucent surfaces.
//!
//! Solid leaves the window exactly as it is created (opaque, no backdrop), so the
//! experiment costs nothing when it's off. Each window applies its own material when
//! it loads and whenever the setting changes.

use tauri::WebviewWindow;
use tauri::window::Color;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND, DWM_SYSTEMBACKDROP_TYPE, DWMSBT_AUTO, DWMSBT_TRANSIENTWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DwmEnableBlurBehindWindow, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject};
use windows::Win32::UI::Controls::MARGINS;

/// The window background before the page paints (`backgroundColor` in tauri.conf.json).
const SOLID: Color = Color(9, 9, 11, 255);

pub fn apply(window: &WebviewWindow, acrylic: bool) -> anyhow::Result<()> {
    let hwnd = HWND(window.hwnd()?.0);
    // SAFETY: DWM attribute calls on our own window handle; the region is freed after use.
    unsafe {
        // Honour the page's alpha: transparent pixels show the backdrop. Solid undoes it.
        let region = CreateRectRgn(0, 0, -1, -1);
        let blur = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION,
            fEnable: acrylic.into(),
            hRgnBlur: region,
            fTransitionOnMaximized: false.into(),
        };
        let blurred = DwmEnableBlurBehindWindow(hwnd, &blur);
        let _ = DeleteObject(region.into());
        blurred?;
        // The backdrop is drawn in the frame; acrylic extends it under the whole page.
        let extent = if acrylic { -1 } else { 0 };
        DwmExtendFrameIntoClientArea(
            hwnd,
            &MARGINS {
                cxLeftWidth: extent,
                cxRightWidth: extent,
                cyTopHeight: extent,
                cyBottomHeight: extent,
            },
        )?;
        // Windows 11 22H2 and later; older systems keep the solid look.
        let backdrop: DWM_SYSTEMBACKDROP_TYPE = if acrylic { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_AUTO };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const backdrop).cast(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        );
    }
    window.set_background_color(Some(if acrylic { Color(0, 0, 0, 0) } else { SOLID }))?;
    Ok(())
}

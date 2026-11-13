//! Window material (Settings → Appearance), experimental: solid, or the system's frosted
//! backdrop (a blur of whatever is behind the window) showing through the page's
//! translucent surfaces: acrylic on Windows, vibrancy (`NSVisualEffectView`) on a Mac.
//!
//! For the backdrop to show, windows are created transparent (see `tauri.conf.json` and
//! the detached-window builder). Solid fills the page with an opaque `--bg`, so the
//! default looks and performs like an ordinary opaque window; acrylic turns the page's
//! backgrounds translucent and lets the system draw the blur behind them.
//!
//! On Windows 11 the acrylic tint follows the window's immersive dark-mode flag, so we
//! set it to match the app theme — otherwise the system draws a light-gray acrylic that
//! reads gray under our dark surfaces. The blur itself is `window-vibrancy` (a tauri
//! dependency). Cosmetic: a failure is logged, never surfaced.
//!
//! Revert by removing this module, its command, the `windowMaterial` setting, the
//! `data-material` CSS, and the windows' `transparent` flag.

use tauri::WebviewWindow;
use tauri::window::Color;

/// The opaque page background in solid mode (matches `--bg`, `backgroundColor` in config).
const SOLID: Color = Color(9, 9, 11, 255);

pub fn apply(window: &WebviewWindow, acrylic: bool, dark: bool) {
    if let Err(error) = try_apply(window, acrylic, dark) {
        tracing::warn!(window = window.label(), %error, "window material not applied");
    }
}

#[cfg(windows)]
fn try_apply(window: &WebviewWindow, acrylic: bool, dark: bool) -> anyhow::Result<()> {
    use window_vibrancy::{apply_acrylic, clear_acrylic};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};

    // The acrylic tint follows this: without it the system backdrop is light-gray.
    // The attribute is a 4-byte BOOL (0/1).
    let immersive_dark: u32 = dark.into();
    // SAFETY: a DWM attribute set on our own window handle; the value outlives the call.
    unsafe {
        let _ = DwmSetWindowAttribute(
            HWND(window.hwnd()?.0),
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const immersive_dark).cast(),
            size_of::<u32>() as u32,
        );
    }
    if acrylic {
        apply_acrylic(window, None)?;
    } else {
        // Best-effort: fails only where acrylic was never applied, which is fine.
        let _ = clear_acrylic(window);
    }
    // Transparent lets the backdrop through; opaque fills the window in solid mode.
    window.set_background_color(Some(if acrylic { Color(0, 0, 0, 0) } else { SOLID }))?;
    Ok(())
}

/// Must run on the main thread (AppKit): `window_set_material` is a sync command, which Tauri runs
/// there.
#[cfg(target_os = "macos")]
fn try_apply(window: &WebviewWindow, acrylic: bool, dark: bool) -> anyhow::Result<()> {
    use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState, apply_vibrancy, clear_vibrancy};

    if acrylic {
        // The material is light or dark with the window's appearance: match the app's theme, or a
        // light system would draw a pale backdrop under our dark surfaces. (Back to following the
        // system when solid.)
        window.set_theme(Some(if dark { tauri::Theme::Dark } else { tauri::Theme::Light }))?;
        apply_vibrancy(
            window,
            NSVisualEffectMaterial::UnderWindowBackground,
            Some(NSVisualEffectState::FollowsWindowActiveState),
            None,
        )?;
    } else {
        // Best-effort: fails only where vibrancy was never applied, which is fine.
        let _ = clear_vibrancy(window);
        window.set_theme(None)?;
    }
    // Transparent lets the backdrop through; opaque fills the window in solid mode.
    window.set_background_color(Some(if acrylic { Color(0, 0, 0, 0) } else { SOLID }))?;
    Ok(())
}

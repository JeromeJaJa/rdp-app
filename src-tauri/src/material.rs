//! Window material (Settings → Appearance), experimental: solid, or the system acrylic
//! backdrop (a blur of whatever is behind the window) showing through the page's
//! translucent surfaces.
//!
//! For the backdrop to show, windows are created transparent (see `tauri.conf.json` and
//! the detached-window builder). Solid fills the page with an opaque `--bg`, so the
//! default looks and performs like an ordinary opaque window; acrylic turns the page's
//! backgrounds translucent and lets the system draw the blur behind them.
//!
//! The backdrop itself is `window-vibrancy` (a tauri dependency); this module only
//! chooses it and keeps the webview's own background in step. Cosmetic: a failure is
//! logged, never surfaced.
//!
//! Revert by removing this module, its command, the `windowMaterial` setting, the
//! `data-material` CSS, and the windows' `transparent` flag.

use tauri::WebviewWindow;
use tauri::window::Color;
use window_vibrancy::{apply_acrylic, clear_acrylic};

/// The opaque page background in solid mode (matches `--bg`, `backgroundColor` in config).
const SOLID: Color = Color(9, 9, 11, 255);

pub fn apply(window: &WebviewWindow, acrylic: bool) {
    if let Err(error) = try_apply(window, acrylic) {
        tracing::warn!(window = window.label(), %error, "window material not applied");
    }
}

fn try_apply(window: &WebviewWindow, acrylic: bool) -> anyhow::Result<()> {
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

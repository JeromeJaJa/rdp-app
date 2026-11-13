//! What the shell needs from whichever way frames reach the page: WebView2 shared buffers on
//! Windows (`frames.rs`), or pages pulling them (`frames_pull.rs`) where there are none.

/// The window that holds the grid, the REST API's view, and any tab not torn off.
pub const MAIN_WINDOW: &str = "main";

pub trait FrameHost: Send + Sync {
    /// Sends `label` the surfaces it shows again (its page loaded or reloaded).
    fn repost_all(&self, label: &str) -> anyhow::Result<()>;
    /// Shows a session's full surface in `label` (its tab moved there).
    fn set_host(&self, session: &str, label: &str);
}

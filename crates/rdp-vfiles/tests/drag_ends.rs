//! A drag started from a thread that never saw the button pressed ends once the button
//! isn't down, instead of running forever and holding the mouse (which stopped every
//! other app from dragging until ours quit).
//!
//! On a window station and desktop of its own: nothing there takes drops, and its input
//! never reaches the user's desktop. One test per file (the station is per process).
#![cfg(windows)]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use rdp_companion_proto::Entry;
use rdp_vfiles::{Options, VirtualFiles};
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, CreateWindowStationW, DESKTOP_CONTROL_FLAGS, SetProcessWindowStation, SetThreadDesktop,
};
use windows::core::w;

struct OneFile(Arc<[Entry]>);

impl VirtualFiles for OneFile {
    fn entries(&self) -> Result<Arc<[Entry]>, String> {
        Ok(Arc::clone(&self.0))
    }

    fn size(&self, _index: u32) -> Result<u64, String> {
        Ok(5)
    }

    fn read(&self, _index: u32, position: u64, out: &mut [u8]) -> Result<usize, String> {
        let data = b"hello";
        let from = (position as usize).min(data.len());
        let n = (data.len() - from).min(out.len());
        out[..n].copy_from_slice(&data[from..from + n]);
        Ok(n)
    }

    fn release(&self, _index: u32) {}
}

#[test]
fn a_drag_without_the_button_ends() {
    // SAFETY: switching this process to a window station of its own, and this thread to a
    // desktop named like the default one, so the drag's thread joins it too.
    unsafe {
        let station = CreateWindowStationW(None, 0, 0x37F /* WINSTA_ALL_ACCESS */, None).unwrap();
        SetProcessWindowStation(station).unwrap();
        let desktop = CreateDesktopW(
            w!("Default"),
            None,
            None,
            DESKTOP_CONTROL_FLAGS(0),
            0x01FF | 0x000F_0000, // DESKTOP_ALL_ACCESS
            None,
        )
        .unwrap();
        SetThreadDesktop(desktop).unwrap();
    }
    let entries: Arc<[Entry]> = vec![Entry {
        path: "a.txt".into(),
        dir: false,
        size: 5,
        modified: 0,
    }]
    .into();
    let (tx, rx) = mpsc::channel();
    rdp_vfiles::drag(Arc::new(OneFile(entries)), Options::default(), move |effect| {
        let _ = tx.send(effect);
    })
    .unwrap();
    let effect = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the drag ended on its own");
    // Nothing here takes files.
    assert_eq!(effect, 0);
}

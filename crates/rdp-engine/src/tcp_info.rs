//! What the session's TCP connection is doing, for the traffic log: the round trip, the
//! windows, and what held sending back. A copy to the remote that crawls on a fast link is
//! held back by one of three things, and they need different fixes: the server's receive
//! window (it won't take more in flight), congestion (losses on the path), or this side not
//! having data queued.

/// A snapshot of a connection's TCP state (Windows' `SIO_TCP_INFO`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TcpInfo {
    rtt_us: u32,
    min_rtt_us: u32,
    cwnd: u32,
    /// The server's receive window: how much it lets this side have in flight.
    peer_window: u32,
    in_flight: u32,
    /// This side's receive window (what the server may have in flight to us).
    window: u32,
    retrans_bytes: u32,
    bytes_out: u64,
    /// Cumulative milliseconds sending was held back by the server's window, by the
    /// congestion window, or by having nothing to send.
    limited_rwin_ms: u32,
    limited_cwnd_ms: u32,
    limited_snd_ms: u32,
}

/// The state of TCP socket `socket` (a raw `SOCKET`), or `None` if Windows won't say.
#[cfg(windows)]
pub(crate) fn query(socket: u64) -> Option<TcpInfo> {
    use windows::Win32::Networking::WinSock::{SIO_TCP_INFO, SOCKET, TCP_INFO_v1, WSAIoctl};

    let version: u32 = 1;
    let mut info = TCP_INFO_v1::default();
    let mut returned = 0u32;
    // SAFETY: an ioctl on our own connected socket, with buffers of the sizes passed.
    let status = unsafe {
        WSAIoctl(
            SOCKET(socket as usize),
            SIO_TCP_INFO,
            Some((&raw const version).cast()),
            size_of::<u32>() as u32,
            Some((&raw mut info).cast()),
            size_of::<TCP_INFO_v1>() as u32,
            &mut returned,
            None,
            None,
        )
    };
    (status == 0).then_some(TcpInfo {
        rtt_us: info.RttUs,
        min_rtt_us: info.MinRttUs,
        cwnd: info.Cwnd,
        peer_window: info.SndWnd,
        in_flight: info.BytesInFlight,
        window: info.RcvWnd,
        retrans_bytes: info.BytesRetrans,
        bytes_out: info.BytesOut,
        limited_rwin_ms: info.SndLimTimeRwin,
        limited_cwnd_ms: info.SndLimTimeCwnd,
        limited_snd_ms: info.SndLimTimeSnd,
    })
}

#[cfg(not(windows))]
pub(crate) fn query(_socket: u64) -> Option<TcpInfo> {
    None
}

/// How a connection did between two snapshots, for the log.
pub(crate) struct Interval {
    pub now: TcpInfo,
    pub before: TcpInfo,
}

impl core::fmt::Display for Interval {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (now, before) = (&self.now, &self.before);
        let kib = |b: u32| f64::from(b) / 1024.0;
        let held = |a: u32, b: u32| a.saturating_sub(b);
        let rwin = held(now.limited_rwin_ms, before.limited_rwin_ms);
        let cwnd = held(now.limited_cwnd_ms, before.limited_cwnd_ms);
        let snd = held(now.limited_snd_ms, before.limited_snd_ms);
        let total = f64::from((rwin + cwnd + snd).max(1));
        let share = |ms: u32| f64::from(ms) * 100.0 / total;
        write!(
            f,
            "rtt={:.0}ms (min {:.0}ms) server_window={:.0}KiB cwnd={:.0}KiB in_flight={:.0}KiB \
             our_window={:.0}KiB sending held by: server_window {:.0}% congestion {:.0}% nothing_queued {:.0}% \
             retransmitted={:.0}KiB of {:.0}KiB",
            f64::from(now.rtt_us) / 1000.0,
            f64::from(now.min_rtt_us) / 1000.0,
            kib(now.peer_window),
            kib(now.cwnd),
            kib(now.in_flight),
            kib(now.window),
            share(rwin),
            share(cwnd),
            share(snd),
            kib(now.retrans_bytes.saturating_sub(before.retrans_bytes)),
            now.bytes_out.saturating_sub(before.bytes_out) as f64 / 1024.0,
        )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::net::{TcpListener, TcpStream};
    use std::os::windows::io::AsRawSocket;

    use super::*;

    #[test]
    fn reads_a_live_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let _server = listener.accept().unwrap();
        let info = query(client.as_raw_socket()).expect("SIO_TCP_INFO");
        assert!(info.peer_window > 0 && info.cwnd > 0, "{info:?}");
        let line = Interval {
            now: info,
            before: info,
        }
        .to_string();
        assert!(line.contains("server_window="), "{line}");
    }
}

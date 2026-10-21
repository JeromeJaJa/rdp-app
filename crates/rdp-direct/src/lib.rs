//! A direct, encrypted link between the RDP App client and its companion, beside the RDP
//! connection, for bulk transfers.
//!
//! Why: Windows' RDP listener keeps its TCP receive window at 64 KiB whatever the host's
//! TCP tuning, so everything sent to the remote over RDP is capped at 64 KiB per round
//! trip (under 1 MiB/s at 70 ms), on any link. A connection of our own has no such cap.
//!
//! How, without opening a port (the way Tailscale or WebRTC connect two machines):
//!
//! 1. Each side binds a UDP socket and lists the addresses it might be reached at
//!    ([`Prepared`]): its interfaces, and the public address a STUN server sees.
//! 2. They swap those lists, and a fingerprint of a certificate made for this link,
//!    over the RDP channel (already authenticated and encrypted).
//! 3. The companion ([`accept`]) sends small "openers" to each of the client's
//!    addresses, so replies from there pass its firewall and NAT; the client
//!    ([`connect`]) tries every companion address at once over QUIC, and the first that
//!    answers wins. Outbound traffic only: nothing listens for strangers.
//! 4. QUIC's TLS 1.3 encrypts the link; each side accepts only the certificate whose
//!    fingerprint came over RDP, so nobody else can join it.
//!
//! The STUN server only sees an address asked about; no data goes through it. If no
//! address pair works (both ends behind strict NATs), callers carry on over RDP.
//!
//! The link carries the same [`Message`]s as the companion channel, in order, on one
//! stream ([`DirectLink`]).

mod stun;
mod tls;

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use quinn::{IdleTimeout, TransportConfig, VarInt};
use rdp_companion_proto::{Decoder, Message, encode};
use tokio::sync::mpsc;

pub use tls::Identity;

/// Public STUN servers asked for this side's public address (the first to answer wins).
pub const STUN_SERVERS: &[&str] = &["stun.cloudflare.com:3478", "stun.l.google.com:19302"];

/// How long the two sides try to reach each other before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

const ALPN: &[u8] = b"rdpapp-direct/1";
/// The name the client asks for (the certificate is checked by fingerprint, not name).
const SERVER_NAME: &str = "rdpapp-companion";
/// Openers go out this often while the companion waits for the client.
const OPENER_EVERY: Duration = Duration::from_millis(200);

/// A bound UDP socket, the addresses it might be reached at, and this link's certificate.
pub struct Prepared {
    socket: UdpSocket,
    identity: Identity,
    candidates: Vec<SocketAddr>,
}

impl Prepared {
    /// Binds a socket and gathers its addresses: each interface's, and the public one the
    /// first answering STUN server in `stun` sees (none if `stun` is empty or none answer).
    pub fn new(stun: &[&str]) -> anyhow::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").context("bind a UDP socket")?;
        let port = socket.local_addr()?.port();
        let mut candidates: Vec<SocketAddr> = local_addresses()
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        if let Some(public) = stun::public_address(&socket, stun)
            && !candidates.contains(&public)
        {
            candidates.push(public);
        }
        Ok(Self {
            socket,
            identity: Identity::generate()?,
            candidates,
        })
    }

    /// Bound to loopback only, without STUN (for tests).
    pub fn loopback() -> anyhow::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let addr = socket.local_addr()?;
        Ok(Self {
            socket,
            identity: Identity::generate()?,
            candidates: vec![addr],
        })
    }

    /// Where the peer might reach this side.
    pub fn candidates(&self) -> &[SocketAddr] {
        &self.candidates
    }

    /// Fingerprint of this side's certificate, for the peer to pin.
    pub fn fingerprint(&self) -> [u8; 32] {
        self.identity.fingerprint()
    }

    /// The port the socket is bound to.
    pub fn port(&self) -> u16 {
        self.socket.local_addr().map_or(0, |a| a.port())
    }
}

/// The far side, as it described itself over RDP.
#[derive(Debug, Clone)]
pub struct Peer {
    pub fingerprint: [u8; 32],
    pub candidates: Vec<SocketAddr>,
}

/// IPv4 addresses of this machine's interfaces a peer might reach (not loopback or
/// link-local). Includes VPN interfaces such as Tailscale's.
fn local_addresses() -> Vec<IpAddr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out: Vec<IpAddr> = interfaces
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
            IpAddr::V6(_) => false,
        })
        .collect();
    out.dedup();
    out
}

/// What a link delivers: each message from the far side, in order.
pub type OnMessage = Box<dyn FnMut(Message) + Send>;
/// The link closed (by either side, or the network); `reason` says why.
pub type OnClosed = Box<dyn FnOnce(String) + Send>;

/// An established direct link. Cheap to clone; sends never block.
#[derive(Clone)]
pub struct DirectLink {
    inner: Arc<Inner>,
}

struct Inner {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    up: Arc<AtomicBool>,
    connection: quinn::Connection,
}

impl DirectLink {
    /// Queues a message for the far side; false if the link is down.
    pub fn send(&self, message: &Message) -> bool {
        self.is_up() && self.inner.frames.send(encode(message)).is_ok()
    }

    pub fn is_up(&self) -> bool {
        self.inner.up.load(Ordering::SeqCst)
    }

    /// The far side's address.
    pub fn remote(&self) -> SocketAddr {
        self.inner.connection.remote_address()
    }

    /// The current round trip estimate.
    pub fn rtt(&self) -> Duration {
        self.inner.connection.rtt()
    }

    /// Closes the link (the far side's `on_closed` runs too).
    pub fn close(&self) {
        self.inner.up.store(false, Ordering::SeqCst);
        self.inner.connection.close(VarInt::from_u32(0), b"closed");
    }
}

impl core::fmt::Debug for DirectLink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DirectLink")
            .field("remote", &self.remote())
            .field("up", &self.is_up())
            .finish()
    }
}

#[derive(Clone, Copy)]
enum Role {
    /// The companion: opens its firewall towards the client, then waits for it.
    Accept,
    /// The client: tries the companion's addresses.
    Connect,
}

/// The companion's side: waits (up to `timeout`) for the client to connect from one of
/// `peer`'s addresses, sending openers towards them meanwhile. Blocks until connected or
/// failed; the link then runs on a thread of its own.
pub fn accept(
    prepared: Prepared,
    peer: Peer,
    timeout: Duration,
    on_message: OnMessage,
    on_closed: OnClosed,
) -> anyhow::Result<DirectLink> {
    establish(Role::Accept, prepared, peer, timeout, on_message, on_closed)
}

/// The client's side: tries each of `peer`'s addresses at once (up to `timeout`).
/// Blocks until connected or failed; the link then runs on a thread of its own.
pub fn connect(
    prepared: Prepared,
    peer: Peer,
    timeout: Duration,
    on_message: OnMessage,
    on_closed: OnClosed,
) -> anyhow::Result<DirectLink> {
    establish(Role::Connect, prepared, peer, timeout, on_message, on_closed)
}

fn establish(
    role: Role,
    prepared: Prepared,
    peer: Peer,
    timeout: Duration,
    on_message: OnMessage,
    on_closed: OnClosed,
) -> anyhow::Result<DirectLink> {
    anyhow::ensure!(!peer.candidates.is_empty(), "the other side has no addresses");
    let (result_tx, result_rx) = std_mpsc::sync_channel(1);
    let name = match role {
        Role::Accept => "direct-accept",
        Role::Connect => "direct-connect",
    };
    std::thread::Builder::new().name(name.into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = result_tx.send(Err(anyhow!(error)));
                return;
            }
        };
        runtime.block_on(run(role, prepared, peer, timeout, result_tx, on_message, on_closed));
    })?;
    result_rx.recv().map_err(|_| anyhow!("the link thread ended"))?
}

async fn run(
    role: Role,
    prepared: Prepared,
    peer: Peer,
    timeout: Duration,
    result: std_mpsc::SyncSender<anyhow::Result<DirectLink>>,
    mut on_message: OnMessage,
    on_closed: OnClosed,
) {
    let opened = open(role, prepared, &peer, timeout).await;
    let (connection, mut send, mut recv, endpoint) = match opened {
        Ok(parts) => parts,
        Err(error) => {
            let _ = result.send(Err(error));
            return;
        }
    };
    let up = Arc::new(AtomicBool::new(true));
    let (frames, mut queued) = mpsc::unbounded_channel::<Vec<u8>>();
    let link = DirectLink {
        inner: Arc::new(Inner {
            frames,
            up: Arc::clone(&up),
            connection: connection.clone(),
        }),
    };
    tracing::info!(remote = %connection.remote_address(), rtt_ms = connection.rtt().as_millis() as u64, "direct link up");
    let _ = result.send(Ok(link));

    let writer = async {
        while let Some(frame) = queued.recv().await {
            send.write_all(&frame).await.map_err(|e| format!("send: {e}"))?;
        }
        Err::<(), String>("closed here".into())
    };
    let reader = async {
        let mut decoder = Decoder::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    decoder.feed(&buf[..n]);
                    loop {
                        match decoder.next_message() {
                            Ok(Some(message)) => on_message(message),
                            Ok(None) => break,
                            Err(error) => return Err::<(), String>(error.to_string()),
                        }
                    }
                }
                Ok(None) => return Err("the other side finished".into()),
                Err(error) => return Err(format!("receive: {error}")),
            }
        }
    };
    let reason = tokio::select! {
        r = writer => r.err().unwrap_or_default(),
        r = reader => r.err().unwrap_or_default(),
        e = connection.closed() => e.to_string(),
    };
    up.store(false, Ordering::SeqCst);
    connection.close(VarInt::from_u32(0), b"closed");
    tracing::info!(%reason, "direct link down");
    on_closed(reason);
    // Let the close reach the other side before this runtime (and its sends) goes.
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
}

type Opened = (quinn::Connection, quinn::SendStream, quinn::RecvStream, quinn::Endpoint);

async fn open(role: Role, prepared: Prepared, peer: &Peer, timeout: Duration) -> anyhow::Result<Opened> {
    let Prepared { socket, identity, .. } = prepared;
    let deadline = Instant::now() + timeout;
    let transport = Arc::new(transport());
    match role {
        Role::Accept => {
            let opener = socket.try_clone()?;
            let mut server = quinn::ServerConfig::with_crypto(Arc::new(tls::server(&identity, peer.fingerprint)?));
            server.transport_config(Arc::clone(&transport));
            let endpoint = quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                Some(server),
                socket,
                Arc::new(quinn::TokioRuntime),
            )?;
            // Open the way for the client's packets from each of its addresses.
            let targets = peer.candidates.clone();
            let openers = tokio::spawn(async move {
                let _ = opener.set_nonblocking(true);
                while Instant::now() < deadline {
                    for target in &targets {
                        let _ = opener.send_to(&[0], target);
                    }
                    tokio::time::sleep(OPENER_EVERY).await;
                }
            });
            let accepted = tokio::time::timeout_at(deadline.into(), async {
                loop {
                    let incoming = endpoint.accept().await.context("the endpoint closed")?;
                    match incoming.await {
                        Ok(connection) => {
                            let (send, recv) = connection.accept_bi().await?;
                            return anyhow::Ok((connection, send, recv));
                        }
                        // Someone without the pinned certificate, or a handshake that broke.
                        Err(error) => tracing::debug!(%error, "direct connection refused"),
                    }
                }
            })
            .await;
            openers.abort();
            let (connection, send, recv) = accepted.map_err(|_| anyhow!("the client didn't reach us"))??;
            Ok((connection, send, recv, endpoint))
        }
        Role::Connect => {
            let mut endpoint = quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                None,
                socket,
                Arc::new(quinn::TokioRuntime),
            )?;
            let mut client = quinn::ClientConfig::new(Arc::new(tls::client(&identity, peer.fingerprint)?));
            client.transport_config(Arc::clone(&transport));
            endpoint.set_default_client_config(client);
            // Every address at once; the first to complete wins.
            let (won_tx, mut won_rx) = mpsc::unbounded_channel();
            for &target in &peer.candidates {
                let connecting = match endpoint.connect(target, SERVER_NAME) {
                    Ok(connecting) => connecting,
                    Err(error) => {
                        tracing::debug!(%target, %error, "can't try this address");
                        continue;
                    }
                };
                let won = won_tx.clone();
                tokio::spawn(async move {
                    let _ = won.send(connecting.await.map_err(|e| (target, e)));
                });
            }
            drop(won_tx);
            let connection = tokio::time::timeout_at(deadline.into(), async {
                while let Some(result) = won_rx.recv().await {
                    match result {
                        Ok(connection) => return Some(connection),
                        Err((target, error)) => tracing::debug!(%target, %error, "direct address failed"),
                    }
                }
                None
            })
            .await
            .ok()
            .flatten()
            .context("no address of the companion answered")?;
            // Late winners close as their handles drop.
            drop(won_rx);
            let (mut send, recv) = connection.open_bi().await?;
            // A stream only reaches the other side once it carries something.
            send.write_all(&encode(&Message::Ping { nonce: 0 })).await?;
            Ok((connection, send, recv, endpoint))
        }
    }
}

/// Windows sized for a fast link with a long round trip (1 Gbit/s at 100 ms is 12.5 MB in
/// flight), and a keep-alive so idle NAT mappings stay open.
fn transport() -> TransportConfig {
    let mut t = TransportConfig::default();
    t.stream_receive_window(VarInt::from_u32(32 << 20));
    t.receive_window(VarInt::from_u32(64 << 20));
    t.send_window(64 << 20);
    t.max_concurrent_uni_streams(VarInt::from_u32(0));
    // A path that dies quietly is given up on soon: transfers then carry on over RDP.
    t.max_idle_timeout(IdleTimeout::try_from(Duration::from_secs(10)).ok());
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t
}

/// The messages that ride the direct link while it's up: a transfer's data and the
/// fetches and acks steering it. Everything else stays on the companion channel.
pub fn rides_direct(message: &Message) -> bool {
    matches!(
        message,
        Message::Data { .. }
            | Message::End { .. }
            | Message::Unreadable { .. }
            | Message::Fetch { .. }
            | Message::Ack { .. }
    )
}

/// One side's direct link, while it's up.
#[derive(Clone, Default)]
pub struct DirectSlot(Arc<Mutex<Option<DirectLink>>>);

impl DirectSlot {
    pub fn set(&self, link: Option<DirectLink>) {
        let old = std::mem::replace(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner), link);
        if let Some(old) = old {
            old.close();
        }
    }

    pub fn get(&self) -> Option<DirectLink> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(DirectLink::is_up)
    }

    /// Sends `message` over the direct link if it rides there and the link is up. False:
    /// send it over the companion channel.
    pub fn try_send(&self, message: &Message) -> bool {
        rides_direct(message) && self.get().is_some_and(|link| link.send(message))
    }

    /// Closes the link, if there is one.
    pub fn close(&self) {
        self.set(None);
    }
}

/// Parses `host:port` candidate strings from the far side, keeping those that parse.
pub fn parse_candidates(list: &[String]) -> Vec<SocketAddr> {
    list.iter().filter_map(|c| c.parse().ok()).collect()
}

/// A fingerprint from the far side, if it's the right length.
pub fn parse_fingerprint(bytes: &[u8]) -> anyhow::Result<[u8; 32]> {
    match <[u8; 32]>::try_from(bytes) {
        Ok(f) => Ok(f),
        Err(_) => bail!("a fingerprint is 32 bytes, got {}", bytes.len()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn data(i: u32, len: usize) -> Message {
        Message::Data {
            id: 2,
            seq: 1,
            index: 0,
            offset: u64::from(i) * len as u64,
            bytes: (0..len).map(|b| (b as u32 ^ i) as u8).collect(),
            compressed: false,
        }
    }

    /// A pair joined over loopback; `received` collects what the client side gets.
    fn pair() -> (
        DirectLink,
        DirectLink,
        std_mpsc::Receiver<Message>,
        Arc<Mutex<Option<String>>>,
    ) {
        let a = Prepared::loopback().unwrap();
        let b = Prepared::loopback().unwrap();
        let to_b = Peer {
            fingerprint: b.fingerprint(),
            candidates: b.candidates().to_vec(),
        };
        let to_a = Peer {
            fingerprint: a.fingerprint(),
            candidates: a.candidates().to_vec(),
        };
        let (tx, rx) = std_mpsc::channel();
        let closed = Arc::new(Mutex::new(None));
        let closed_at_server = Arc::clone(&closed);
        let server = std::thread::spawn(move || {
            accept(
                a,
                to_b,
                Duration::from_secs(10),
                Box::new(|_| {}),
                Box::new(move |why| *closed_at_server.lock().unwrap() = Some(why)),
            )
        });
        let client = connect(
            b,
            to_a,
            Duration::from_secs(10),
            Box::new(move |m| {
                let _ = tx.send(m);
            }),
            Box::new(|_| {}),
        )
        .unwrap();
        let server = server.join().unwrap().unwrap();
        (server, client, rx, closed)
    }

    #[test]
    fn carries_messages_in_order_and_fast() {
        let (server, client, received, _) = pair();
        assert!(server.is_up() && client.is_up());
        const N: u32 = 2048;
        const LEN: usize = 64 * 1024;
        let started = Instant::now();
        for i in 0..N {
            assert!(server.send(&data(i, LEN)));
        }
        for i in 0..N {
            let message = received.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(message, data(i, LEN), "message {i}");
        }
        let mb_per_s = (u64::from(N) * LEN as u64) as f64 / 1e6 / started.elapsed().as_secs_f64();
        assert!(mb_per_s > 100.0, "{mb_per_s:.0} MB/s over loopback");
    }

    #[test]
    fn a_wrong_certificate_is_refused() {
        let a = Prepared::loopback().unwrap();
        let b = Prepared::loopback().unwrap();
        let impostor = Identity::generate().unwrap();
        let to_b = Peer {
            fingerprint: impostor.fingerprint(),
            candidates: b.candidates().to_vec(),
        };
        let to_a = Peer {
            fingerprint: a.fingerprint(),
            candidates: a.candidates().to_vec(),
        };
        let server =
            std::thread::spawn(move || accept(a, to_b, Duration::from_secs(3), Box::new(|_| {}), Box::new(|_| {})));
        let client = connect(b, to_a, Duration::from_secs(3), Box::new(|_| {}), Box::new(|_| {}));
        assert!(client.is_err() || server.join().unwrap().is_err());
    }

    #[test]
    fn closing_tells_the_other_side() {
        let (server, client, _received, closed) = pair();
        client.close();
        let deadline = Instant::now() + Duration::from_secs(5);
        while closed.lock().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(closed.lock().unwrap().is_some(), "the server heard the close");
        assert!(!server.is_up());
        assert!(!server.send(&Message::Ping { nonce: 1 }));
    }
}

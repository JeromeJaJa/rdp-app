//! Just enough STUN (RFC 8489) to learn this socket's public address: a Binding request,
//! and the XOR-MAPPED-ADDRESS in the answer.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const MAPPED_ADDRESS: u16 = 0x0001;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
/// Per server, across retries.
const WAIT: Duration = Duration::from_millis(1500);

/// The address the first answering server in `servers` sees `socket`'s datagrams come
/// from (IPv4 only, like the socket). `None` if none answers.
pub(crate) fn public_address(socket: &UdpSocket, servers: &[&str]) -> Option<SocketAddr> {
    let previous = socket.read_timeout().ok().flatten();
    let found = servers.iter().find_map(|server| {
        let target = server.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)?;
        let found = ask(socket, target);
        match found {
            Some(addr) => tracing::debug!(%server, %addr, "public address"),
            None => tracing::debug!(%server, "no STUN answer"),
        }
        found
    });
    let _ = socket.set_read_timeout(previous);
    found
}

fn ask(socket: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let mut transaction = [0u8; 12];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut transaction)
        .ok()?;
    let request = request(&transaction);
    let deadline = Instant::now() + WAIT;
    let mut buf = [0u8; 512];
    // A few sends, in case one is lost.
    while Instant::now() < deadline {
        socket.send_to(&request, server).ok()?;
        let resend_at = (Instant::now() + Duration::from_millis(300)).min(deadline);
        while let Some(left) = resend_at
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
        {
            socket.set_read_timeout(Some(left)).ok()?;
            match socket.recv_from(&mut buf) {
                Ok((n, from)) if from == server => {
                    if let Some(addr) = parse(&buf[..n], &transaction) {
                        return Some(addr);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    None
}

fn request(transaction: &[u8; 12]) -> [u8; 20] {
    let mut out = [0u8; 20];
    out[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    // Message length 0: no attributes.
    out[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    out[8..20].copy_from_slice(transaction);
    out
}

/// The mapped address in a Binding success response to `transaction`.
fn parse(message: &[u8], transaction: &[u8; 12]) -> Option<SocketAddr> {
    if message.len() < 20
        || u16::from_be_bytes([message[0], message[1]]) != BINDING_SUCCESS
        || u32::from_be_bytes(message[4..8].try_into().ok()?) != MAGIC_COOKIE
        || &message[8..20] != transaction
    {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([message[2], message[3]]));
    let attributes = message.get(20..20 + length)?;
    let mut plain = None;
    let mut at = 0;
    while at + 4 <= attributes.len() {
        let kind = u16::from_be_bytes([attributes[at], attributes[at + 1]]);
        let len = usize::from(u16::from_be_bytes([attributes[at + 2], attributes[at + 3]]));
        let value = attributes.get(at + 4..at + 4 + len)?;
        match kind {
            XOR_MAPPED_ADDRESS => return address(value, Some(transaction)),
            MAPPED_ADDRESS => plain = address(value, None),
            _ => {}
        }
        // Attributes are padded to 4 bytes.
        at += 4 + len.div_ceil(4) * 4;
    }
    plain
}

/// A (XOR-)MAPPED-ADDRESS value; `xor` carries the transaction id for IPv6 addresses.
fn address(value: &[u8], xor: Option<&[u8; 12]>) -> Option<SocketAddr> {
    if value.len() < 4 {
        return None;
    }
    let cookie = MAGIC_COOKIE.to_be_bytes();
    let mut port = u16::from_be_bytes([value[2], value[3]]);
    if xor.is_some() {
        port ^= (MAGIC_COOKIE >> 16) as u16;
    }
    let ip = match value[1] {
        0x01 => {
            let mut b: [u8; 4] = value.get(4..8)?.try_into().ok()?;
            if xor.is_some() {
                b.iter_mut().zip(cookie).for_each(|(x, c)| *x ^= c);
            }
            IpAddr::V4(Ipv4Addr::from(b))
        }
        0x02 => {
            let mut b: [u8; 16] = value.get(4..20)?.try_into().ok()?;
            if let Some(transaction) = xor {
                let key: Vec<u8> = cookie.iter().chain(transaction.iter()).copied().collect();
                b.iter_mut().zip(key).for_each(|(x, k)| *x ^= k);
            }
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_xor_mapped_address() {
        // RFC 5769 2.2: a response with XOR-MAPPED-ADDRESS 192.0.2.1:32853.
        let transaction = [0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae];
        let mut message = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        message.extend_from_slice(&transaction);
        message.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43]);
        assert_eq!(parse(&message, &transaction), Some("192.0.2.1:32853".parse().unwrap()));
        // Another transaction's answer is ignored.
        assert_eq!(parse(&message, &[0; 12]), None);
    }

    #[test]
    fn answers_from_a_local_server() {
        // A one-shot STUN "server" that reflects the sender's address.
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            assert_eq!(n, 20);
            let mut reply = vec![0x01, 0x01, 0x00, 0x0c];
            reply.extend_from_slice(&buf[4..20]);
            let SocketAddr::V4(from4) = from else { panic!() };
            let port = from4.port() ^ (MAGIC_COOKIE >> 16) as u16;
            let mut ip = from4.ip().octets();
            ip.iter_mut().zip(MAGIC_COOKIE.to_be_bytes()).for_each(|(x, c)| *x ^= c);
            reply.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            reply.extend_from_slice(&port.to_be_bytes());
            reply.extend_from_slice(&ip);
            server.send_to(&reply, from).unwrap();
        });
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let seen = public_address(&socket, &[&server_addr.to_string()]);
        assert_eq!(seen, Some(socket.local_addr().unwrap()));
    }
}

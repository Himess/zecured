//! Zcash Testnet block headers from the peer-to-peer network: version handshake, `getheaders`,
//! `headers`. Read-only (nothing is ever relayed). A Rust port of
//! `contracts/test/relay/tools/zcash_p2p_headers.py`.
//!
//! Light wallet servers' compact blocks carry no full header, and a history-tree leaf needs every
//! block's nBits, so the witness builder needs the headers of the whole NU7 epoch. Nothing here
//! has to be trusted: every header's encoding, SHA-256d hash and link to the previous one are
//! checked, the history root built from them must match the anchor header's commitments, and
//! the anchor itself is checked on Base by the relay.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::history::{HEADER_LENGTH, sha256d};

const TESTNET_MAGIC: [u8; 4] = [0xfa, 0x1a, 0xf9, 0xbf];
const TESTNET_PORT: u16 = 18_233;
/// ZIP 259: MIN_NETWORK_PROTOCOL_VERSION (NU7) on Testnet.
const PROTOCOL_VERSION: i32 = 170_180;
const SEEDERS: [&str; 2] = ["dnsseed.testnet.z.cash", "testnet.seeder.zfnd.org"];
const USER_AGENT: &[u8] = b"/zecured-witness:0.1/";
const PREFIX_LENGTH: usize = 140;
const SOLUTION_LENGTH: usize = 1344;
const MAX_MESSAGE: usize = 8_000_000;
/// Many seeded addresses do not answer: give up on a connection quickly, on a reply slowly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(30);

/// One line of a header file: `{ height, hash (display order), raw (hex) }`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderLine {
    pub height: u32,
    pub hash: String,
    pub raw: String,
}

fn varint(n: u64) -> Vec<u8> {
    match n {
        0..0xfd => vec![n as u8],
        0xfd..=0xffff => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
        0x1_0000..=0xffff_ffff => [&[0xfe][..], &(n as u32).to_le_bytes()].concat(),
        _ => [&[0xff][..], &n.to_le_bytes()].concat(),
    }
}

fn read_varint(buf: &[u8], pos: usize) -> Result<(u64, usize)> {
    let first = *buf.get(pos).context("truncated compactSize")?;
    let take = |n: usize| -> Result<&[u8]> { buf.get(pos + 1..pos + 1 + n).context("truncated compactSize") };
    Ok(match first {
        0xfd => (u64::from(u16::from_le_bytes(take(2)?.try_into()?)), pos + 3),
        0xfe => (u64::from(u32::from_le_bytes(take(4)?.try_into()?)), pos + 5),
        0xff => (u64::from_le_bytes(take(8)?.try_into()?), pos + 9),
        b => (u64::from(b), pos + 1),
    })
}

fn net_addr(ip: Option<std::net::Ipv4Addr>, port: u16) -> Vec<u8> {
    let mut out = 0u64.to_le_bytes().to_vec(); // services
    out.extend_from_slice(&[0u8; 10]);
    out.extend_from_slice(&[0xff, 0xff]);
    out.extend_from_slice(&ip.map_or([0; 4], |a| a.octets()));
    out.extend_from_slice(&port.to_be_bytes());
    out
}

/// A connection to one Testnet peer.
pub struct Peer {
    stream: TcpStream,
    buf: Vec<u8>,
    /// The peer's user agent, from its `version` message.
    pub agent: String,
}

impl Peer {
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).context("connect")?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        let mut peer = Peer {
            stream,
            buf: vec![],
            agent: String::new(),
        };
        peer.handshake(addr)?;
        Ok(peer)
    }

    fn send(&mut self, command: &str, payload: &[u8]) -> Result<()> {
        let mut name = [0u8; 12];
        name[..command.len()].copy_from_slice(command.as_bytes());
        let mut msg = TESTNET_MAGIC.to_vec();
        msg.extend_from_slice(&name);
        msg.extend_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
        msg.extend_from_slice(&sha256d(payload)[..4]);
        msg.extend_from_slice(payload);
        self.stream.write_all(&msg).context("send")
    }

    fn recv(&mut self) -> Result<(String, Vec<u8>)> {
        loop {
            if self.buf.len() >= 24 {
                ensure!(self.buf[..4] == TESTNET_MAGIC, "bad network magic from peer");
                let length = u32::from_le_bytes(self.buf[16..20].try_into()?) as usize;
                ensure!(length <= MAX_MESSAGE, "oversized message");
                if self.buf.len() >= 24 + length {
                    let command = String::from_utf8_lossy(&self.buf[4..16]).trim_end_matches('\0').to_owned();
                    let checksum: [u8; 4] = self.buf[20..24].try_into()?;
                    let payload = self.buf[24..24 + length].to_vec();
                    self.buf.drain(..24 + length);
                    ensure!(sha256d(&payload)[..4] == checksum, "bad message checksum");
                    return Ok((command, payload));
                }
            }
            let mut chunk = [0u8; 1 << 16];
            let n = self.stream.read(&mut chunk).context("receive")?;
            ensure!(n > 0, "peer closed the connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn handshake(&mut self, addr: SocketAddr) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let ip = match addr {
            SocketAddr::V4(a) => Some(*a.ip()),
            SocketAddr::V6(_) => None,
        };
        let nonce = rand_core_06::RngCore::next_u64(&mut rand_core_06::OsRng).to_le_bytes();
        let mut payload = PROTOCOL_VERSION.to_le_bytes().to_vec();
        payload.extend_from_slice(&0u64.to_le_bytes()); // services
        payload.extend_from_slice(&i64::try_from(now)?.to_le_bytes());
        payload.extend_from_slice(&net_addr(ip, addr.port()));
        payload.extend_from_slice(&net_addr(None, 0));
        payload.extend_from_slice(&nonce);
        payload.extend_from_slice(&varint(USER_AGENT.len() as u64));
        payload.extend_from_slice(USER_AGENT);
        payload.extend_from_slice(&0i32.to_le_bytes()); // start height
        payload.push(0); // relay = false
        self.send("version", &payload)?;
        let (mut version, mut verack) = (false, false);
        while !(version && verack) {
            let (command, body) = self.recv()?;
            match command.as_str() {
                "version" => {
                    let (len, pos) = read_varint(&body, 80)?;
                    let agent = body.get(pos..pos + len as usize).context("truncated version message")?;
                    self.agent = String::from_utf8_lossy(agent).into_owned();
                    version = true;
                    self.send("verack", &[])?;
                }
                "verack" => verack = true,
                "ping" => self.send("pong", &body)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// The headers a peer sends after `locator` (internal byte order), up to 2,000.
    pub fn get_headers(&mut self, locator: [u8; 32]) -> Result<Vec<Vec<u8>>> {
        let mut payload = PROTOCOL_VERSION.to_le_bytes().to_vec();
        payload.extend_from_slice(&varint(1));
        payload.extend_from_slice(&locator);
        payload.extend_from_slice(&[0u8; 32]);
        self.send("getheaders", &payload)?;
        loop {
            let (command, body) = self.recv()?;
            match command.as_str() {
                "headers" => return parse_headers(&body),
                "ping" => self.send("pong", &body)?,
                _ => {}
            }
        }
    }
}

/// The 1487-byte headers of a `headers` message (each followed by a zero transaction count).
fn parse_headers(body: &[u8]) -> Result<Vec<Vec<u8>>> {
    let (count, mut pos) = read_varint(body, 0)?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (solution, at) = read_varint(body, pos + PREFIX_LENGTH)?;
        ensure!(
            solution as usize == SOLUTION_LENGTH && at == pos + PREFIX_LENGTH + 3,
            "unexpected solution encoding"
        );
        let raw = body.get(pos..at + SOLUTION_LENGTH).context("truncated header")?;
        ensure!(raw.len() == HEADER_LENGTH, "truncated header");
        out.push(raw.to_vec());
        let (transactions, next) = read_varint(body, at + SOLUTION_LENGTH)?;
        ensure!(transactions == 0, "headers message with transactions");
        pos = next;
    }
    ensure!(pos == body.len(), "trailing bytes in headers message");
    Ok(out)
}

/// Testnet peers from the DNS seeders.
pub fn seed_peers() -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = vec![];
    for seeder in SEEDERS {
        if let Ok(addrs) = (seeder, TESTNET_PORT).to_socket_addrs() {
            for a in addrs.filter(SocketAddr::is_ipv4) {
                if !out.contains(&a) {
                    out.push(a);
                }
            }
        }
    }
    out
}

fn display(hash: &[u8; 32]) -> String {
    let mut h = *hash;
    h.reverse();
    hex::encode(h)
}

/// Up to `count` headers after the block `(from_height, from_hash)` (internal byte order), each
/// linked to the previous one; fewer if the peers reach their tip. Tries `peers` in turn.
pub fn fetch(from_height: u32, from_hash: [u8; 32], count: usize, peers: &[SocketAddr]) -> Result<Vec<HeaderLine>> {
    let mut out = vec![];
    let (mut height, mut locator) = (from_height, from_hash);
    let mut candidates = peers.iter();
    let mut peer: Option<Peer> = None;
    while out.len() < count {
        let p = match peer.as_mut() {
            Some(p) => p,
            None => {
                let addr = candidates.next().context("ran out of Testnet peers")?;
                match Peer::connect(*addr) {
                    Ok(p) => peer.insert(p),
                    Err(_) => continue,
                }
            }
        };
        let raws = match p.get_headers(locator) {
            Ok(r) => r,
            Err(_) => {
                peer = None;
                continue;
            }
        };
        if raws.is_empty() {
            break; // the peer's tip
        }
        if raws[0][4..36] != locator {
            // This peer does not continue from our block (it is behind, or on another branch).
            peer = None;
            continue;
        }
        for raw in raws {
            ensure!(raw[4..36] == locator, "headers not linked at height {}", height + 1);
            let hash = sha256d(&raw);
            height += 1;
            out.push(HeaderLine {
                height,
                hash: display(&hash),
                raw: hex::encode(&raw),
            });
            locator = hash;
            if out.len() >= count {
                break;
            }
        }
    }
    Ok(out)
}

/// Appends to the JSON Lines header file at `path` the headers that follow its last line, up
/// to the peers' tip, and returns the new last height. The file must already hold at least one
/// header (the start of the epoch, fetched once).
pub fn extend_file(path: &Path, peers: &[SocketAddr]) -> Result<u32> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let last: HeaderLine =
        serde_json::from_str(text.lines().rev().find(|l| !l.trim().is_empty()).context("empty header file")?)?;
    let mut hash: [u8; 32] = hex::decode(&last.hash)?.try_into().map_err(|_| anyhow!("bad hash"))?;
    hash.reverse();
    let mut height = last.height;
    let mut file = OpenOptions::new().append(true).open(path)?;
    loop {
        let batch = fetch(height, hash, 2_000, peers)?;
        let Some(end) = batch.last() else { break };
        let mut lines = String::new();
        for h in &batch {
            lines.push_str(&serde_json::to_string(h)?);
            lines.push('\n');
        }
        file.write_all(lines.as_bytes())?;
        height = end.height;
        hash = hex::decode(&end.hash)?.try_into().map_err(|_| anyhow!("bad hash"))?;
        hash.reverse();
        if batch.len() < 2_000 {
            break;
        }
    }
    Ok(height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_sizes_round_trip() {
        for n in [0u64, 1, 0xfc, 0xfd, 0xffff, 0x1_0000, 0xffff_ffff, 0x1_0000_0000] {
            let enc = varint(n);
            assert_eq!(read_varint(&enc, 0).unwrap(), (n, enc.len()), "{n}");
        }
        assert!(read_varint(&[0xfd, 1], 0).is_err());
    }

    #[test]
    fn headers_messages_are_checked() {
        let header = vec![0u8; PREFIX_LENGTH]
            .into_iter()
            .chain([0xfd, 0x40, 0x05])
            .chain(vec![7u8; SOLUTION_LENGTH])
            .collect::<Vec<u8>>();
        let mut body = varint(2);
        for _ in 0..2 {
            body.extend_from_slice(&header);
            body.push(0);
        }
        assert_eq!(parse_headers(&body).unwrap(), vec![header.clone(), header.clone()]);
        let mut with_tx = body.clone();
        with_tx[1 + HEADER_LENGTH] = 1;
        assert!(parse_headers(&with_tx).is_err());
        assert!(parse_headers(&body[..body.len() - 1]).is_err());
    }
}

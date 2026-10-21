//! Zcash block headers from the peer-to-peer network of the deployment's Zcash network (Testnet or
//! Mainnet): version handshake, `getheaders`, `headers`. Read-only (nothing is ever relayed). A
//! Rust port of `contracts/test/relay/tools/zcash_p2p_headers.py`.
//!
//! Light wallet servers' compact blocks carry no full header, and a history-tree leaf needs every
//! block's nBits, so the witness builder needs the headers of the whole epoch. Nothing here
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
use zecured_zcash_ops::network::ZcashNetwork;

use crate::history::{HEADER_LENGTH, sha256d};

/// A Zcash network's message magic, default port and DNS seeders, from Zebra v7.0.0-rc.0
/// (docs/ref/FACTS.md): `zebra-chain/src/parameters/constants.rs` (`magics`), `network.rs`
/// (`default_port`) and `zebra-network/src/config.rs` (`DEFAULT_MAINNET_PEERS`; for Testnet the two
/// seeders this client always used).
struct NetworkParams {
    magic: [u8; 4],
    port: u16,
    seeders: &'static [&'static str],
}

fn network_params(network: ZcashNetwork) -> NetworkParams {
    match network {
        ZcashNetwork::Testnet => NetworkParams {
            magic: [0xfa, 0x1a, 0xf9, 0xbf],
            port: 18_233,
            seeders: &["dnsseed.testnet.z.cash", "testnet.seeder.zfnd.org"],
        },
        ZcashNetwork::Mainnet => NetworkParams {
            magic: [0x24, 0xe9, 0x27, 0x64],
            port: 8_233,
            seeders: &[
                "dnsseed.str4d.xyz",
                "dnsseed.z.cash",
                "mainnet.seeder.shieldedinfra.net",
                "mainnet.seeder.zfnd.org",
                "seeder.zec.rocks",
            ],
        },
    }
}

/// The protocol version announced, on both networks (Zebra 7.0.0-rc.0's own): ZIP 259's NU7
/// version on Testnet; Mainnet peers require at least NU6.3's 170,160 until Mainnet's NU7, whose
/// version is 170,190 (docs/MAINNET.md § 5).
const PROTOCOL_VERSION: i32 = 170_180;
const USER_AGENT: &[u8] = b"/zecured-witness:0.1/";
const PREFIX_LENGTH: usize = 140;
const SOLUTION_LENGTH: usize = 1344;
const MAX_MESSAGE: usize = 8_000_000;
/// Many seeded addresses do not answer: give up on a connection quickly, on a reply slowly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(30);
/// A peer that keeps the connection alive (pings, inventory) but never answers `getheaders`
/// must not hold the caller forever: give up on the answer after this long (a responsive peer
/// answers in about a second).
const HEADERS_DEADLINE: Duration = Duration::from_secs(15);

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

/// A connection to one peer of a Zcash network.
pub struct Peer {
    stream: TcpStream,
    /// The network's message magic: a peer of another network is refused at its first message.
    magic: [u8; 4],
    buf: Vec<u8>,
    /// The peer's user agent, from its `version` message.
    pub agent: String,
    /// The best height the peer announced in its `version` message (its word only).
    pub start_height: u32,
}

impl Peer {
    /// Connects to `addr`, a peer of Zcash `network`.
    pub fn connect(network: ZcashNetwork, addr: SocketAddr) -> Result<Self> {
        Self::connect_within(network, addr, TIMEOUT)
    }

    /// [`Peer::connect`], with reads and writes timing out after `io_timeout`.
    pub fn connect_within(network: ZcashNetwork, addr: SocketAddr, io_timeout: Duration) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).context("connect")?;
        stream.set_read_timeout(Some(io_timeout))?;
        stream.set_write_timeout(Some(io_timeout))?;
        let mut peer = Peer {
            stream,
            magic: network_params(network).magic,
            buf: vec![],
            agent: String::new(),
            start_height: 0,
        };
        peer.handshake(addr)?;
        Ok(peer)
    }

    fn send(&mut self, command: &str, payload: &[u8]) -> Result<()> {
        let mut name = [0u8; 12];
        name[..command.len()].copy_from_slice(command.as_bytes());
        let mut msg = self.magic.to_vec();
        msg.extend_from_slice(&name);
        msg.extend_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
        msg.extend_from_slice(&sha256d(payload)[..4]);
        msg.extend_from_slice(payload);
        self.stream.write_all(&msg).context("send")
    }

    fn recv(&mut self) -> Result<(String, Vec<u8>)> {
        loop {
            if self.buf.len() >= 24 {
                ensure!(self.buf[..4] == self.magic, "bad network magic from peer");
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
                    let end = pos.checked_add(usize::try_from(len)?).context("truncated version message")?;
                    let agent = body.get(pos..end).context("truncated version message")?;
                    self.agent = String::from_utf8_lossy(agent).into_owned();
                    let height = body.get(end..end + 4).context("truncated version message")?;
                    self.start_height = u32::try_from(i32::from_le_bytes(height.try_into()?)).unwrap_or(0);
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
        let started = std::time::Instant::now();
        loop {
            ensure!(started.elapsed() < HEADERS_DEADLINE, "no headers from the peer in time");
            let (command, body) = self.recv()?;
            match command.as_str() {
                "headers" => return parse_headers(&body),
                "ping" => self.send("pong", &body)?,
                _ => {}
            }
        }
    }
}

/// Most headers one `headers` message may carry (the protocol's limit).
const MAX_HEADERS: u64 = 2_000;
/// Most headers zcashd and Zebra send in one answer (`MAX_HEADERS_RESULTS`,
/// `MAX_FIND_BLOCK_HEADERS_RESULTS`): a shorter answer ends at the peer's tip, so asking again would
/// only wait for an answer that does not come.
const PEER_HEADERS_PER_ANSWER: usize = 160;

/// The 1487-byte headers of a `headers` message (each followed by a zero transaction count).
fn parse_headers(body: &[u8]) -> Result<Vec<Vec<u8>>> {
    let (count, mut pos) = read_varint(body, 0)?;
    // The count is the peer's word: never allocate for more headers than the protocol allows or
    // than the message can hold.
    ensure!(count <= MAX_HEADERS, "headers message claims {count} headers");
    ensure!(count as usize <= body.len() / HEADER_LENGTH, "headers message too short for {count} headers");
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

/// Peers of Zcash `network` from its DNS seeders.
pub fn seed_peers(network: ZcashNetwork) -> Vec<SocketAddr> {
    let params = network_params(network);
    let mut out: Vec<SocketAddr> = vec![];
    for seeder in params.seeders {
        if let Ok(addrs) = (*seeder, params.port).to_socket_addrs() {
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

/// A peer that completed a handshake, and the best height it announced in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reachable {
    pub addr: SocketAddr,
    pub height: u32,
}

/// Handshake time a probed peer gets: a peer slower than this is no use to a feeder.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connects to all `peers` (of Zcash `network`) at once and returns those that complete a handshake
/// within a few seconds, highest announced height first. Most seed addresses are unreachable at any
/// time; probing them in parallel costs one timeout instead of one per address.
pub fn probe(network: ZcashNetwork, peers: &[SocketAddr]) -> Vec<Reachable> {
    let found = std::sync::Mutex::new(Vec::with_capacity(peers.len()));
    std::thread::scope(|scope| {
        for addr in peers {
            let found = &found;
            scope.spawn(move || {
                if let Ok(p) = Peer::connect_within(network, *addr, PROBE_TIMEOUT) {
                    found.lock().unwrap_or_else(|e| e.into_inner()).push(Reachable {
                        addr: *addr,
                        height: p.start_height,
                    });
                }
            });
        }
    });
    let mut out = found.into_inner().unwrap_or_else(|e| e.into_inner());
    out.sort_by_key(|p| std::cmp::Reverse(p.height));
    out
}

/// The highest best height that up to `count` reachable peers announce, among the first
/// `3 * count` addresses: an estimate of the Zcash tip (each peer's word only), `None` if none
/// answers.
pub fn announced_height(network: ZcashNetwork, peers: &[SocketAddr], count: usize) -> Option<u32> {
    peers
        .iter()
        .take(3 * count)
        .filter_map(|a| Peer::connect(network, *a).ok())
        .take(count)
        .map(|p| p.start_height)
        .max()
}

/// Up to `count` headers after the block `(from_height, from_hash)` (internal byte order) of Zcash
/// `network`, each linked to the previous one; fewer if the peers reach their tip. Tries `peers`
/// in turn.
pub fn fetch(
    network: ZcashNetwork,
    from_height: u32,
    from_hash: [u8; 32],
    count: usize,
    peers: &[SocketAddr],
) -> Result<Vec<HeaderLine>> {
    fetch_with_peer(network, from_height, from_hash, count, peers).map(|(lines, _)| lines)
}

/// [`fetch`], also returning the peer that answered last (a long-running feeder asks it first next
/// time instead of waiting on unreachable seed addresses again).
pub fn fetch_with_peer(
    network: ZcashNetwork,
    from_height: u32,
    from_hash: [u8; 32],
    count: usize,
    peers: &[SocketAddr],
) -> Result<(Vec<HeaderLine>, Option<SocketAddr>)> {
    let mut out = vec![];
    let (mut height, mut locator) = (from_height, from_hash);
    let mut candidates = peers.iter();
    let mut peer: Option<(SocketAddr, Peer)> = None;
    let mut answered = None;
    while out.len() < count {
        let (addr, p) = match peer.as_mut() {
            Some((a, p)) => (*a, p),
            None => {
                let addr = *candidates.next().with_context(|| format!("ran out of Zcash {network} peers"))?;
                match Peer::connect(network, addr) {
                    Ok(p) => {
                        let (_, p) = peer.insert((addr, p));
                        (addr, p)
                    }
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
            if p.start_height > height {
                // It announced more blocks, but none after ours: it is on another branch.
                peer = None;
                continue;
            }
            answered = Some(addr);
            break; // the peer's tip
        }
        if raws[0][4..36] != locator {
            // This peer does not continue from our block (it is behind, or on another branch).
            peer = None;
            continue;
        }
        answered = Some(addr);
        let at_tip = raws.len() < PEER_HEADERS_PER_ANSWER;
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
        if at_tip {
            break;
        }
    }
    Ok((out, answered))
}

/// Appends to the JSON Lines header file at `path` (headers of Zcash `network`) the headers that
/// follow its last line, up to the peers' tip, and returns the new last height. The file must
/// already hold at least one header (the start of the epoch, fetched once).
pub fn extend_file(network: ZcashNetwork, path: &Path, peers: &[SocketAddr]) -> Result<u32> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let last: HeaderLine =
        serde_json::from_str(text.lines().rev().find(|l| !l.trim().is_empty()).context("empty header file")?)?;
    let mut hash: [u8; 32] = hex::decode(&last.hash)?.try_into().map_err(|_| anyhow!("bad hash"))?;
    hash.reverse();
    let mut height = last.height;
    let mut file = OpenOptions::new().append(true).open(path)?;
    loop {
        let batch = fetch(network, height, hash, 2_000, peers)?;
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

    /// Live, by hand: two relayer-like fetches from a recent block, the second asking the peer
    /// that answered the first (`ZECURED_TIP=height:hash` in internal byte order, 0x-prefixed).
    #[test]
    #[ignore = "needs Testnet peers"]
    fn live_relayer_fetch_timing() {
        let tip = std::env::var("ZECURED_TIP").expect("ZECURED_TIP=height:0xhash");
        let (height, hash) = tip.split_once(':').unwrap();
        let hash: [u8; 32] = hex::decode(hash.trim_start_matches("0x")).unwrap().try_into().unwrap();
        let mut peers = seed_peers(ZcashNetwork::Testnet);
        for round in 0..2 {
            let t = std::time::Instant::now();
            let (lines, peer) =
                fetch_with_peer(ZcashNetwork::Testnet, height.parse().unwrap(), hash, 400, &peers).unwrap();
            println!("round {round}: {} headers from {peer:?} in {:?}", lines.len(), t.elapsed());
            if let Some(p) = peer {
                peers.retain(|a| *a != p);
                peers.insert(0, p);
            }
        }
    }

    /// Live, by hand: where a header fetch spends its time (`cargo test -p zecured-deposit-witness
    /// live_fetch_timing -- --ignored --nocapture`).
    #[test]
    #[ignore = "needs Testnet peers"]
    fn live_fetch_timing() {
        let started = std::time::Instant::now();
        let peers = seed_peers(ZcashNetwork::Testnet);
        println!("{} seed addresses in {:?}", peers.len(), started.elapsed());
        let mut answered = 0;
        for addr in &peers {
            if answered == 3 {
                break;
            }
            let t = std::time::Instant::now();
            match Peer::connect(ZcashNetwork::Testnet, *addr) {
                Ok(mut p) => {
                    answered += 1;
                    println!("{addr}: handshake {:?}, agent {}, height {}", t.elapsed(), p.agent, p.start_height);
                    let t = std::time::Instant::now();
                    // The genesis of the NU7 epoch is far behind: a full answer comes back.
                    let r = p.get_headers([0u8; 32]);
                    println!("{addr}: getheaders {:?} -> {:?}", t.elapsed(), r.map(|h| h.len()));
                }
                Err(e) => println!("{addr}: {e} after {:?}", t.elapsed()),
            }
        }
    }

    #[test]
    fn compact_sizes_round_trip() {
        for n in [0u64, 1, 0xfc, 0xfd, 0xffff, 0x1_0000, 0xffff_ffff, 0x1_0000_0000] {
            let enc = varint(n);
            assert_eq!(read_varint(&enc, 0).unwrap(), (n, enc.len()), "{n}");
        }
        assert!(read_varint(&[0xfd, 1], 0).is_err());
    }

    /// Review finding: a peer's claimed header count must not size an allocation.
    #[test]
    fn a_claimed_header_count_is_checked_before_allocating() {
        let huge = [&[0xff][..], &u64::MAX.to_le_bytes()].concat();
        assert!(parse_headers(&huge).is_err());
        let too_many = [&[0xfd][..], &2_001u16.to_le_bytes()].concat();
        assert!(parse_headers(&too_many).is_err());
        // 2,000 claimed, no data.
        let empty = [&[0xfd][..], &2_000u16.to_le_bytes()].concat();
        assert!(parse_headers(&empty).is_err());
        assert_eq!(parse_headers(&[0]).unwrap(), Vec::<Vec<u8>>::new());
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

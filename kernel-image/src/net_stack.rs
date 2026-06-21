//! NIC-backed network transport: ties the boot-installed [`NetDevice`] together
//! with the from-scratch `cibos-net` protocol logic to provide UDP datagram
//! send/receive. This is the transport the Lattice's remote Links sit on
//! (N5) — the Gate/Link/Warden surface is unchanged; only the byte path beneath
//! a Link can now be a NIC instead of loopback.
//!
//! Scope (honest): UDP first (datagram Links), plus ARP resolution and ICMP echo
//! handling for verifiability. TCP is a separate later milestone. The host
//! configuration (our IP, gateway) is set at bring-up; a DHCP client can replace
//! the static config later.

#![cfg(all(target_os = "none", target_arch = "x86_64"))]
// This module is the NIC-backed transport API. Its public surface (udp_send_to,
// poll_udp, is_configured, TransportError) is exercised now by the boot UDP
// self-check and is the seam the Lattice's remote Links bind to next (routing a
// Link's bytes over UDP instead of loopback). Until that final wiring lands,
// some items are only reached via the demo self-check; allow that honestly
// rather than fake-wiring them.
#![allow(dead_code)]

use cibos_kernel::net_device::NetDevice;
use cibos_kernel::sync::SpinLock;
use cibos_net::{arp, ethernet, ipv4, udp};
use cibos_net::{ethertype, ip_proto, Ipv4Addr, MacAddr, NetError, MAC_BROADCAST};

/// Host network configuration (static for now; DHCP can replace it later).
#[derive(Clone, Copy)]
struct NetConfig {
    mac: MacAddr,
    ip: Ipv4Addr,
    gateway: Ipv4Addr,
    netmask: [u8; 4],
}

/// The stack state: our config + the ARP cache. Guarded by a spinlock; brief
/// holds only.
struct NetStack {
    cfg: NetConfig,
    arp_cache: arp::ArpCache,
}

static STACK: SpinLock<Option<NetStack>> = SpinLock::new(None);

/// Errors from the NIC-backed transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// No NIC is installed (loopback-only host).
    NoNic,
    /// The stack has not been configured (no IP).
    Unconfigured,
    /// ARP resolution for the next hop did not complete in the budget.
    ArpTimeout,
    /// A protocol build/parse error.
    Net(NetError),
    /// The device transmit/receive failed.
    Device,
}

impl From<NetError> for TransportError {
    fn from(e: NetError) -> Self {
        TransportError::Net(e)
    }
}

/// Initialize the stack's host configuration. Called at bring-up once a NIC is
/// present, with the NIC's MAC. Uses the standard QEMU user-net addressing by
/// default (IP 10.0.2.15, gateway 10.0.2.2); a DHCP client can override later.
pub fn configure(mac: MacAddr) {
    let cfg = NetConfig {
        mac,
        ip: Ipv4Addr::new(10, 0, 2, 15),
        gateway: Ipv4Addr::new(10, 0, 2, 2),
        netmask: [255, 255, 255, 0],
    };
    *STACK.lock() = Some(NetStack {
        cfg,
        arp_cache: arp::ArpCache::new(),
    });
}

/// Whether the stack is configured.
#[must_use]
pub fn is_configured() -> bool {
    STACK.lock().is_some()
}

/// Decide the next-hop IPv4 for `dst`: `dst` itself if on-link (same subnet),
/// else the gateway.
fn next_hop(cfg: &NetConfig, dst: Ipv4Addr) -> Ipv4Addr {
    let on_link = (0..4).all(|i| {
        (dst.0[i] & cfg.netmask[i]) == (cfg.ip.0[i] & cfg.netmask[i])
    });
    if on_link {
        dst
    } else {
        cfg.gateway
    }
}

/// Resolve `ip` to a MAC, using the cache or sending an ARP request and polling
/// for the reply on the NIC. Brief, bounded.
fn resolve(nic: &dyn NetDevice, stack: &mut NetStack, ip: Ipv4Addr) -> Result<MacAddr, TransportError> {
    if let Some(mac) = stack.arp_cache.lookup(ip) {
        return Ok(mac);
    }
    // Build + send an ARP request.
    let req = arp::request(stack.cfg.mac, stack.cfg.ip, ip);
    let mut arp_buf = [0u8; arp::ARP_LEN];
    req.build(&mut arp_buf)?;
    let mut frame = [0u8; ethernet::ETH_HEADER_LEN + arp::ARP_LEN];
    let n = ethernet::build(
        &mut frame,
        MAC_BROADCAST,
        stack.cfg.mac,
        ethertype::ARP,
        &arp_buf,
    )?;
    nic.send_frame(&frame[..n]).map_err(|_| TransportError::Device)?;

    // Poll for the ARP reply.
    let mut rx = [0u8; 1514];
    for _ in 0..2_000_000u64 {
        if let Ok(Some(len)) = nic.recv_frame(&mut rx) {
            if let Ok(eth) = ethernet::EthFrame::parse(&rx[..len]) {
                if eth.ethertype == ethertype::ARP {
                    if let Ok(pkt) = arp::ArpPacket::parse(eth.payload) {
                        // Learn any reply; specifically resolve our target.
                        if pkt.oper == arp::oper::REPLY {
                            stack.arp_cache.insert(pkt.sender_ip, pkt.sender_mac);
                            if pkt.sender_ip == ip {
                                return Ok(pkt.sender_mac);
                            }
                        }
                    }
                }
            }
        }
        core::hint::spin_loop();
    }
    Err(TransportError::ArpTimeout)
}

/// A remote Link's backing transport: a UDP flow identified by the local port we
/// listen on and the remote `(ip, port)` peer. Carries a Lattice Link's bytes
/// over the NIC instead of the loopback Channel — the Gate/Link/Warden surface
/// above is unchanged. Datagram semantics: each `send` is one UDP datagram, each
/// `recv` yields at most one datagram's payload (UDP Links; ordered/reliable
/// Links await TCP).
#[derive(Clone, Copy)]
pub struct RemoteLink {
    pub local_port: u16,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
}

impl RemoteLink {
    /// Send `bytes` as one UDP datagram to the peer.
    ///
    /// # Errors
    /// [`TransportError`] on missing NIC/config, ARP timeout, or device failure.
    pub fn send(&self, bytes: &[u8]) -> Result<usize, TransportError> {
        udp_send_to(self.remote_ip, self.remote_port, self.local_port, bytes)
    }

    /// Receive at most one datagram addressed to our local port into `out`.
    /// Returns `Some(len)` if a datagram from our peer arrived, else `None`.
    ///
    /// # Errors
    /// [`TransportError`] on missing NIC/config or device failure.
    pub fn recv(&self, out: &mut [u8]) -> Result<Option<usize>, TransportError> {
        match poll_udp(self.local_port, out)? {
            Some((src_ip, src_port, len))
                if src_ip == self.remote_ip && src_port == self.remote_port =>
            {
                Ok(Some(len))
            }
            // A datagram for our port but from a different peer is not this Link's.
            _ => Ok(None),
        }
    }
}
///
/// # Errors
/// [`TransportError`] variants for missing NIC, unconfigured stack, ARP timeout,
/// protocol build errors, or device failure.
pub fn udp_send_to(
    dst_ip: Ipv4Addr,
    dst_port: u16,
    src_port: u16,
    payload: &[u8],
) -> Result<usize, TransportError> {
    let mut guard = STACK.lock();
    let stack = guard.as_mut().ok_or(TransportError::Unconfigured)?;
    let hop = next_hop(&stack.cfg, dst_ip);

    crate::boot::with_nic(|nic| {
        let dst_mac = resolve(nic, stack, hop)?;
        // Build UDP into a scratch buffer, then IPv4, then Ethernet.
        let mut udp_buf = [0u8; 1480];
        let udp_len = udp::build(
            &mut udp_buf,
            stack.cfg.ip,
            dst_ip,
            src_port,
            dst_port,
            payload,
        )?;
        let mut ip_buf = [0u8; 1500];
        let ip_len = ipv4::build(
            &mut ip_buf,
            stack.cfg.ip,
            dst_ip,
            ip_proto::UDP,
            64,
            0,
            &udp_buf[..udp_len],
        )?;
        let mut frame = [0u8; 1514];
        let n = ethernet::build(
            &mut frame,
            dst_mac,
            stack.cfg.mac,
            ethertype::IPV4,
            &ip_buf[..ip_len],
        )?;
        nic.send_frame(&frame[..n]).map_err(|_| TransportError::Device)?;
        Ok(payload.len())
    })
    .ok_or(TransportError::NoNic)?
}

/// Poll the NIC once for an inbound UDP datagram addressed to `port`. On a match,
/// copies the payload into `out` and returns `Some((src_ip, src_port, len))`.
/// Also answers ARP requests for our IP and ICMP echo requests (so the host is a
/// good network citizen and is pingable), returning `None` for those.
///
/// # Errors
/// [`TransportError`] for missing NIC/config or device errors.
pub fn poll_udp(
    port: u16,
    out: &mut [u8],
) -> Result<Option<(Ipv4Addr, u16, usize)>, TransportError> {
    let mut guard = STACK.lock();
    let stack = guard.as_mut().ok_or(TransportError::Unconfigured)?;

    let mut rx = [0u8; 1514];
    let result = crate::boot::with_nic(|nic| {
        let len = match nic.recv_frame(&mut rx) {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(None),
            Err(_) => return Err(TransportError::Device),
        };
        let eth = match ethernet::EthFrame::parse(&rx[..len]) {
            Ok(e) => e,
            Err(_) => return Ok(None),
        };
        match eth.ethertype {
            ethertype::ARP => {
                if let Ok(pkt) = arp::ArpPacket::parse(eth.payload) {
                    if pkt.oper == arp::oper::REQUEST && pkt.target_ip == stack.cfg.ip {
                        // Answer the ARP request for our IP.
                        let rep = arp::reply(&pkt, stack.cfg.mac, stack.cfg.ip);
                        let mut ab = [0u8; arp::ARP_LEN];
                        if rep.build(&mut ab).is_ok() {
                            let mut fb = [0u8; ethernet::ETH_HEADER_LEN + arp::ARP_LEN];
                            if let Ok(n) = ethernet::build(
                                &mut fb,
                                pkt.sender_mac,
                                stack.cfg.mac,
                                ethertype::ARP,
                                &ab,
                            ) {
                                let _ = nic.send_frame(&fb[..n]);
                            }
                        }
                    }
                    // Learn the sender either way.
                    if let Ok(p) = arp::ArpPacket::parse(eth.payload) {
                        stack.arp_cache.insert(p.sender_ip, p.sender_mac);
                    }
                }
                Ok(None)
            }
            ethertype::IPV4 => {
                let ip = match ipv4::Ipv4Packet::parse(eth.payload) {
                    Ok(p) => p,
                    Err(_) => return Ok(None),
                };
                if ip.dst != stack.cfg.ip && ip.dst != Ipv4Addr::BROADCAST {
                    return Ok(None);
                }
                match ip.protocol {
                    ip_proto::UDP => {
                        let d = match udp::UdpDatagram::parse(ip.payload, ip.src, ip.dst) {
                            Ok(d) => d,
                            Err(_) => return Ok(None),
                        };
                        if d.dst_port == port {
                            let n = d.payload.len().min(out.len());
                            out[..n].copy_from_slice(&d.payload[..n]);
                            return Ok(Some((ip.src, d.src_port, n)));
                        }
                        Ok(None)
                    }
                    ip_proto::ICMP => {
                        // Answer echo requests so the host is pingable.
                        if let Ok(echo) = cibos_net::icmp::IcmpEcho::parse(ip.payload) {
                            if !echo.is_reply {
                                let mut icmp_buf = [0u8; 1480];
                                if let Ok(il) = cibos_net::icmp::echo_reply(
                                    &mut icmp_buf,
                                    echo.id,
                                    echo.seq,
                                    echo.payload,
                                ) {
                                    let mut ipb = [0u8; 1500];
                                    if let Ok(ipl) = ipv4::build(
                                        &mut ipb,
                                        stack.cfg.ip,
                                        ip.src,
                                        ip_proto::ICMP,
                                        64,
                                        0,
                                        &icmp_buf[..il],
                                    ) {
                                        // Resolve the sender's MAC (likely cached).
                                        if let Ok(mac) = resolve(nic, stack, ip.src) {
                                            let mut fb = [0u8; 1514];
                                            if let Ok(n) = ethernet::build(
                                                &mut fb,
                                                mac,
                                                stack.cfg.mac,
                                                ethertype::IPV4,
                                                &ipb[..ipl],
                                            ) {
                                                let _ = nic.send_frame(&fb[..n]);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None)
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    });
    match result {
        Some(r) => r,
        None => Err(TransportError::NoNic),
    }
}

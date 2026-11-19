//! W1c network isolation fixtures and probes (pure safe Rust; Win32 token checks live in `windows.rs`).
//!
//! Spins up host-local TCP/UDP listeners, establishes positive host controls (confirming the
//! listeners accept host traffic), resets counters to zero, then verifies that the LPAC child
//! gets OS access-denied errors AND zero packets/connections reach any listener.

use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Win32 Winsock `WSAEACCES` (10013): AppContainer / LPAC without network capability.
const WSAEACCES: i32 = 10013;

pub(crate) const NETWORK_EVIDENCE: &str =
    "NET_CAPS=0 NET_TCP4_DENIED=1 NET_UDP4_DENIED=1 NET_IPV6_CHECKED=1";

pub(crate) struct HostListeners {
    tcp4_addr: SocketAddr,
    udp4_addr: SocketAddr,
    tcp6_addr: Option<SocketAddr>,
    tcp4_hits: Arc<AtomicUsize>,
    udp4_hits: Arc<AtomicUsize>,
    tcp6_hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl HostListeners {
    pub(crate) fn start() -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let tcp4_hits = Arc::new(AtomicUsize::new(0));
        let udp4_hits = Arc::new(AtomicUsize::new(0));
        let tcp6_hits = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();

        let tcp4 = TcpListener::bind("127.0.0.1:0")?;
        tcp4.set_nonblocking(true)?;
        let tcp4_addr = tcp4.local_addr()?;
        threads.push(spawn_tcp_counter(
            tcp4,
            Arc::clone(&tcp4_hits),
            Arc::clone(&stop),
        ));

        let udp4 = UdpSocket::bind("127.0.0.1:0")?;
        udp4.set_nonblocking(true)?;
        let udp4_addr = udp4.local_addr()?;
        threads.push(spawn_udp_counter(
            udp4,
            Arc::clone(&udp4_hits),
            Arc::clone(&stop),
        ));

        let tcp6_addr = match TcpListener::bind("[::1]:0") {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                let addr = listener.local_addr()?;
                threads.push(spawn_tcp_counter(
                    listener,
                    Arc::clone(&tcp6_hits),
                    Arc::clone(&stop),
                ));
                Some(addr)
            }
            Err(_) => None,
        };

        let listeners = Self {
            tcp4_addr,
            udp4_addr,
            tcp6_addr,
            tcp4_hits,
            udp4_hits,
            tcp6_hits,
            stop,
            threads,
        };
        listeners.verify_host_controls()?;
        Ok(listeners)
    }

    fn verify_host_controls(&self) -> io::Result<()> {
        // Host TCP4 positive control
        let mut stream = TcpStream::connect_timeout(&self.tcp4_addr, Duration::from_secs(2))?;
        stream.write_all(b"host-ping")?;
        let mut buf = [0u8; 4];
        let _ = stream.read(&mut buf);
        drop(stream);

        // Host UDP4 positive control
        let udp = UdpSocket::bind("127.0.0.1:0")?;
        udp.send_to(b"host-udp", self.udp4_addr)?;

        // Host TCP6 positive control (if IPv6 loopback is enabled on host)
        if let Some(addr6) = self.tcp6_addr {
            let mut s6 = TcpStream::connect_timeout(&addr6, Duration::from_secs(2))?;
            s6.write_all(b"host-ping6")?;
            drop(s6);
        }

        // Wait briefly for counter threads to observe the host controls.
        for _ in 0..40 {
            let ok6 = self.tcp6_addr.is_none() || self.tcp6_hits.load(Ordering::SeqCst) > 0;
            if self.tcp4_hits.load(Ordering::SeqCst) > 0
                && self.udp4_hits.load(Ordering::SeqCst) > 0
                && ok6
            {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if self.tcp4_hits.load(Ordering::SeqCst) == 0 || self.udp4_hits.load(Ordering::SeqCst) == 0
        {
            return Err(io::Error::other(
                "host loopback listener positive control failed",
            ));
        }
        if self.tcp6_addr.is_some() && self.tcp6_hits.load(Ordering::SeqCst) == 0 {
            return Err(io::Error::other(
                "host IPv6 loopback listener positive control failed",
            ));
        }
        // Reset counters before launching the LPAC child.
        self.tcp4_hits.store(0, Ordering::SeqCst);
        self.udp4_hits.store(0, Ordering::SeqCst);
        self.tcp6_hits.store(0, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn tcp4_addr(&self) -> SocketAddr {
        self.tcp4_addr
    }

    pub(crate) fn udp4_addr(&self) -> SocketAddr {
        self.udp4_addr
    }

    pub(crate) fn tcp6_addr(&self) -> Option<SocketAddr> {
        self.tcp6_addr
    }

    pub(crate) fn verify_zero_child_traffic(&self) -> io::Result<()> {
        // Give nonblocking listener loops one brief tick to catch any stray packet.
        thread::sleep(Duration::from_millis(50));
        let tcp4 = self.tcp4_hits.load(Ordering::SeqCst);
        let udp4 = self.udp4_hits.load(Ordering::SeqCst);
        let tcp6 = self.tcp6_hits.load(Ordering::SeqCst);
        if tcp4 != 0 || udp4 != 0 || tcp6 != 0 {
            return Err(io::Error::other(format!(
                "LPAC child reached host listener: tcp4={tcp4} udp4={udp4} tcp6={tcp6}"
            )));
        }
        Ok(())
    }
}

impl Drop for HostListeners {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

fn spawn_tcp_counter(
    listener: TcpListener,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(b"pong");
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    })
}

fn spawn_udp_counter(
    socket: UdpSocket,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 64];
        while !stop.load(Ordering::SeqCst) {
            match socket.recv_from(&mut buf) {
                Ok(_) => {
                    hits.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    })
}

fn is_network_denied(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
        || matches!(
            err.raw_os_error(),
            Some(WSAEACCES) | Some(10051) | Some(10049)
        )
}

/// Runs inside the LPAC child to assert TCP4, UDP4, and optional TCP6 loopback are blocked.
pub(crate) fn child_probe(
    tcp4: &str,
    udp4: &str,
    tcp6: &str,
    capability_count: u32,
) -> io::Result<()> {
    if capability_count != 0 {
        return Err(io::Error::other(format!(
            "expected 0 token capabilities in LPAC, got {capability_count}"
        )));
    }
    let tcp4_addr: SocketAddr = tcp4.parse().map_err(io::Error::other)?;
    let udp4_addr: SocketAddr = udp4.parse().map_err(io::Error::other)?;

    match TcpStream::connect_timeout(&tcp4_addr, Duration::from_secs(2)) {
        Ok(_) => {
            return Err(io::Error::other(
                "LPAC child unexpectedly connected to IPv4 TCP loopback",
            ));
        }
        Err(err) if is_network_denied(&err) => {}
        Err(err) => {
            return Err(io::Error::other(format!(
                "IPv4 TCP loopback failed with non-denial error: {err} (raw={:?})",
                err.raw_os_error()
            )));
        }
    }

    let udp_blocked = match UdpSocket::bind("127.0.0.1:0") {
        Err(err) if is_network_denied(&err) => true,
        Err(err) => {
            return Err(io::Error::other(format!(
                "UDP bind failed with non-denial error: {err} (raw={:?})",
                err.raw_os_error()
            )));
        }
        Ok(sock) => match sock.send_to(b"lpac-udp", udp4_addr) {
            Ok(_) => {
                return Err(io::Error::other(
                    "LPAC child unexpectedly sent UDP datagram to loopback",
                ));
            }
            Err(err) if is_network_denied(&err) => true,
            Err(err) => {
                return Err(io::Error::other(format!(
                    "UDP send failed with non-denial error: {err} (raw={:?})",
                    err.raw_os_error()
                )));
            }
        },
    };
    if !udp_blocked {
        return Err(io::Error::other("UDP denial check did not confirm denial"));
    }

    if tcp6 != "none" {
        let tcp6_addr: SocketAddr = tcp6.parse().map_err(io::Error::other)?;
        match TcpStream::connect_timeout(&tcp6_addr, Duration::from_secs(2)) {
            Ok(_) => {
                return Err(io::Error::other(
                    "LPAC child unexpectedly connected to IPv6 TCP loopback",
                ));
            }
            Err(err) if is_network_denied(&err) => {}
            Err(err) => {
                return Err(io::Error::other(format!(
                    "IPv6 TCP loopback failed with non-denial error: {err} (raw={:?})",
                    err.raw_os_error()
                )));
            }
        }
    }

    println!("{NETWORK_EVIDENCE}");
    Ok(())
}

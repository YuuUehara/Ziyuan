//! 号舍的门吏：按域名放行的 HTTP 代理。
//!
//! 号舍内网络全断，只留通往门吏的一条路；门吏只替名单上的域名转递（`CONNECT` 隧道与
//! 绝对地址的明文 HTTP），其余一律回 403，并记下被拦的域名供审计。
use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 请求头的上限：超过即不是正常的代理请求。
const MAX_HEADER: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 名单上的域名是否放行 `host`：`example.com` 只放行其本身，`*.example.com` 只放行其子域。
pub fn allows(domains: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    domains.iter().any(|entry| {
        let entry = entry.trim_end_matches('.').to_ascii_lowercase();
        match entry.strip_prefix("*.") {
            Some(parent) => host
                .strip_suffix(parent)
                .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
            None => host == entry,
        }
    })
}

/// 正在当值的门吏；丢弃时停止接客（已建立的连接自行结束）。
pub struct Proxy {
    /// 门吏所在：Linux 为 Unix 套接字所在目录（经 socat 接入号舍），其余为本机 TCP 端口。
    pub door: Door,
    denied: Arc<Mutex<BTreeSet<String>>>,
    stop: Arc<AtomicBool>,
}

pub enum Door {
    #[cfg(target_os = "linux")]
    Socket { dir: PathBuf, path: PathBuf },
    #[cfg(not(target_os = "linux"))]
    Port(u16),
}

impl Proxy {
    /// 在 Linux 上开在 Unix 套接字上（号舍内无网络，只能经文件系统接入），其余开在本机端口。
    pub fn start(domains: Vec<String>) -> io::Result<Self> {
        let denied = Arc::new(Mutex::new(BTreeSet::new()));
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "linux")]
        let door = {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ziyuan-haoshe-door-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("door.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path)?;
            let clients = std::iter::from_fn(move || Some(listener.accept().map(|(c, _)| c)));
            keep_door(clients, domains, &denied, &stop);
            Door::Socket { dir, path }
        };
        #[cfg(not(target_os = "linux"))]
        let door = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let clients = std::iter::from_fn(move || Some(listener.accept().map(|(c, _)| c)));
            keep_door(clients, domains, &denied, &stop);
            Door::Port(port)
        };
        Ok(Self { door, denied, stop })
    }

    /// 至今被拦下的域名。
    pub fn denied(&self) -> Vec<String> {
        self.denied
            .lock()
            .expect("门吏记录锁未中毒")
            .iter()
            .cloned()
            .collect()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // 敲一下门，让阻塞在 accept 上的线程醒来看到停止标记。
        match &self.door {
            #[cfg(target_os = "linux")]
            Door::Socket { dir, path } => {
                let _ = std::os::unix::net::UnixStream::connect(path);
                let _ = std::fs::remove_dir_all(dir);
            }
            #[cfg(not(target_os = "linux"))]
            Door::Port(port) => {
                let _ = TcpStream::connect(("127.0.0.1", *port));
            }
        }
    }
}

/// 门吏当值：逐个接客，各开一线程转递，见停止标记即收工。
fn keep_door<S: Duplex>(
    clients: impl Iterator<Item = io::Result<S>> + Send + 'static,
    domains: Vec<String>,
    denied: &Arc<Mutex<BTreeSet<String>>>,
    stop: &Arc<AtomicBool>,
) {
    let (domains, denied, stop) = (Arc::new(domains), denied.clone(), stop.clone());
    std::thread::spawn(move || {
        for client in clients {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(client) = client {
                let (domains, denied) = (domains.clone(), denied.clone());
                std::thread::spawn(move || {
                    let _ = serve(client, &domains, &denied);
                });
            }
        }
    });
}

/// 可双向转递的连接。
trait Duplex: Read + Write + Send + Sized + 'static {
    fn split(&self) -> io::Result<Self>;
    fn close_write(&self);
}

impl Duplex for TcpStream {
    fn split(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn close_write(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

#[cfg(target_os = "linux")]
impl Duplex for std::os::unix::net::UnixStream {
    fn split(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn close_write(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

fn serve<S: Duplex>(
    client: S,
    domains: &[String],
    denied: &Mutex<BTreeSet<String>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(client.split()?);
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if reader.read_until(b'\n', &mut head)? == 0 || head.len() > MAX_HEADER {
            return Ok(());
        }
    }
    let line = String::from_utf8_lossy(&head[..head.iter().position(|&b| b == b'\r').unwrap_or(0)])
        .into_owned();
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let connect = method.eq_ignore_ascii_case("CONNECT");
    let authority = if connect {
        target
    } else {
        let Some(rest) = target.strip_prefix("http://") else {
            return refuse(client, "号舍门吏只转递 CONNECT 与 http:// 绝对地址的请求");
        };
        rest.split('/').next().unwrap_or("")
    };
    let (host, port) = split_authority(authority, if connect { 443 } else { 80 });
    if host.is_empty() || !allows(domains, &host) {
        denied
            .lock()
            .expect("门吏记录锁未中毒")
            .insert(host.clone());
        return refuse(
            client,
            &format!("号舍不放行 {host}；只放行：{}", domains.join("、")),
        );
    }
    let upstream = (host.as_str(), port)
        .to_socket_addrs()?
        .find_map(|addr| TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok());
    let Some(mut upstream) = upstream else {
        return refuse_with(client, "502 Bad Gateway", &format!("连不上 {host}:{port}"));
    };
    let mut client = client;
    if connect {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    } else {
        upstream.write_all(&head)?;
    }
    // 读请求头时多读进来的字节也要转过去。
    upstream.write_all(reader.buffer())?;
    let (mut up_read, up_write) = (upstream.split()?, upstream);
    let pump = std::thread::spawn(move || {
        let mut up_write = up_write;
        let _ = io::copy(&mut reader, &mut up_write);
        up_write.close_write();
    });
    let _ = io::copy(&mut up_read, &mut client);
    client.close_write();
    let _ = pump.join();
    Ok(())
}

/// `host:port` 或 `[v6]:port`，缺端口时取 `default`。
fn split_authority(authority: &str, default: u16) -> (String, u16) {
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').unwrap_or((rest, ""));
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return (host.to_ascii_lowercase(), port.unwrap_or(default));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => {
            (host.to_ascii_lowercase(), port.parse().unwrap_or(default))
        }
        _ => (authority.to_ascii_lowercase(), default),
    }
}

fn refuse<S: Duplex>(client: S, reason: &str) -> io::Result<()> {
    refuse_with(client, "403 Forbidden", reason)
}

fn refuse_with<S: Duplex>(mut client: S, status: &str, reason: &str) -> io::Result<()> {
    write!(
        client,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}",
        reason.len()
    )?;
    client.close_write();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_match_exactly_or_by_wildcard_subdomain() {
        let list = vec!["crates.io".to_owned(), "*.github.com".to_owned()];
        assert!(allows(&list, "crates.io"));
        assert!(allows(&list, "Crates.IO."));
        assert!(!allows(&list, "static.crates.io"), "不带 *. 的只放行其本身");
        assert!(!allows(&list, "evilcrates.io"));
        assert!(allows(&list, "api.github.com"));
        assert!(!allows(&list, "github.com"), "*. 只放行子域");
        assert!(!allows(&list, "evilgithub.com"));
        assert_eq!(split_authority("[::1]:8080", 443), ("::1".into(), 8080));
        assert_eq!(
            split_authority("Example.com", 80),
            ("example.com".into(), 80)
        );
    }
}

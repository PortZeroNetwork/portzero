//! Shared fixtures for the overlay e2e test binaries under `tests/`.
//!
//! Every `.rs` file directly under `tests/` is its own independent
//! integration-test binary, so anything used by more than one of them lives
//! here instead. `tests/common/` is not itself compiled as a test binary;
//! each file that needs these helpers adds `mod common;` and calls
//! `common::helper_name(...)`.
//!
//! Each test binary only pulls in the subset of these helpers it needs, so an
//! individual binary legitimately leaves some items unused — allow dead_code
//! at the module level rather than suppress it per item.
#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use hickory_proto::op::{Message, MessageType, OpCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};

use std::str::FromStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

/// Overall safety net so a stuck test never hangs CI.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Spawn a tiny TCP echo server bound to an ephemeral port (the "port 0"
/// backend a real service would expose). Returns its real address.
pub async fn spawn_echo_backend() -> SocketAddr {
    spawn_echo_backend_on("127.0.0.1:0")
        .await
        .expect("IPv4 loopback is always bindable")
}

/// [`spawn_echo_backend`] on an explicit bind address, for tests that care
/// which address family the backend is listening on. Returns `None` when the
/// host cannot bind it at all (e.g. IPv6 disabled), so the caller can skip
/// rather than fail.
pub async fn spawn_echo_backend_on(bind: &str) -> Option<SocketAddr> {
    let listener = TcpListener::bind(bind).await.ok()?;
    let addr = listener.local_addr().ok()?;
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });
    Some(addr)
}

/// Spawn a TCP backend that answers every connection with `identity` and
/// nothing else, bound to an ephemeral port.
///
/// An echo backend cannot tell you *which* backend answered — the reply is
/// whatever you sent. When several backends claim one tunnel name, that is
/// precisely the question, so this one names itself instead.
pub async fn spawn_identifying_backend(identity: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let _ = sock.write_all(identity.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    addr
}

/// Send a single A query for `name` to the DNS server at `dns_addr` and return
/// the first A record's address, if any. Bounded by `TEST_TIMEOUT`.
pub async fn dns_query_a(dns_addr: SocketAddr, name: &str) -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.connect(dns_addr).await.unwrap();

    let mut req = Message::new(0x1234, MessageType::Query, OpCode::Query);
    req.metadata.recursion_desired = true;
    let mut q = hickory_proto::op::Query::new();
    q.set_name(Name::from_str(name).unwrap());
    q.set_query_type(RecordType::A);
    q.set_query_class(DNSClass::IN);
    req.add_query(q);

    let bytes = req.to_bytes().unwrap();
    sock.send(&bytes).await.unwrap();

    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(TEST_TIMEOUT, sock.recv(&mut buf))
        .await
        .expect("DNS query timed out")
        .expect("DNS recv failed");

    let resp = Message::from_bytes(&buf[..n]).unwrap();
    for ans in &resp.answers {
        if let RData::A(a) = &ans.data {
            return Some(a.0);
        }
    }
    None
}

/// Connect to `addr`, send `payload`, and require the exact echo. Retries are
/// intentionally short-lived: the privileged e2e may race listener installation
/// or route propagation for a few milliseconds after `update_services`.
pub async fn assert_tcp_echo(addr: SocketAddr, payload: &[u8]) {
    let mut last_error = String::new();

    for _ in 0..20 {
        let attempt = tokio::time::timeout(Duration::from_millis(250), async {
            let mut client = tokio::net::TcpStream::connect(addr).await?;
            client.write_all(payload).await?;
            let mut got = vec![0u8; payload.len()];
            client.read_exact(&mut got).await?;
            Ok::<Vec<u8>, std::io::Error>(got)
        })
        .await;

        match attempt {
            Ok(Ok(got)) if got == payload => return,
            Ok(Ok(got)) => {
                last_error = format!("echo mismatch: got {got:?}, expected {payload:?}");
            }
            Ok(Err(e)) => {
                last_error = e.to_string();
            }
            Err(_) => {
                last_error = "connect/read/write timed out".to_string();
            }
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    #[cfg(target_os = "windows")]
    let diagnostics = windows_route_diagnostics(addr);
    #[cfg(not(target_os = "windows"))]
    let diagnostics = String::new();

    panic!("TCP tunnel echo to {addr} failed: {last_error}{diagnostics}");
}

#[cfg(target_os = "windows")]
fn windows_route_diagnostics(addr: SocketAddr) -> String {
    let ip = addr.ip();
    let script = format!(
        r#"
$ErrorActionPreference = "Continue"
$ip = "{ip}"
$alias = "portzero-e2e"
"`n--- portzero-e2e adapter ---"
Get-NetAdapter -Name $alias -ErrorAction SilentlyContinue | Format-List Name,InterfaceIndex,Status,MacAddress,LinkSpeed
"`n--- portzero-e2e addresses ---"
Get-NetIPAddress -InterfaceAlias $alias -AddressFamily IPv4 -ErrorAction SilentlyContinue | Format-List IPAddress,PrefixLength,PrefixOrigin,SuffixOrigin,AddressState
"`n--- route selected for VIP ---"
Find-NetRoute -RemoteIPAddress $ip -ErrorAction SilentlyContinue | Format-List DestinationPrefix,NextHop,InterfaceAlias,InterfaceIndex,RouteMetric,InterfaceMetric,PolicyStore
"`n--- relevant routes ---"
Get-NetRoute -AddressFamily IPv4 -ErrorAction SilentlyContinue |
  Where-Object {{ $_.DestinationPrefix -eq "$ip/32" -or $_.DestinationPrefix -eq "10.254.0.0/16" -or $_.InterfaceAlias -eq $alias }} |
  Sort-Object DestinationPrefix,RouteMetric |
  Format-Table DestinationPrefix,NextHop,InterfaceAlias,InterfaceIndex,RouteMetric,InterfaceMetric,PolicyStore -AutoSize
"`n--- netsh ipv4 config ---"
netsh interface ipv4 show config name="$alias"
"#
    );

    match std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
    {
        Ok(output) => format!(
            "\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(e) => format!("\nfailed to collect Windows route diagnostics: {e}"),
    }
}

#[cfg(target_os = "windows")]
pub struct WindowsHostRoute {
    prefix: String,
    interface_alias: &'static str,
}

#[cfg(target_os = "windows")]
impl WindowsHostRoute {
    pub fn install(ip: Ipv4Addr, interface_alias: &'static str) -> Self {
        let prefix = format!("{ip}/32");
        let script = format!(
            r#"
$ErrorActionPreference = "Stop"
$prefix = "{prefix}"
$alias = "{interface_alias}"
Remove-NetRoute -DestinationPrefix $prefix -InterfaceAlias $alias -Confirm:$false -ErrorAction SilentlyContinue
$adapter = Get-NetAdapter -Name $alias -ErrorAction Stop
New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $adapter.InterfaceIndex -NextHop 0.0.0.0 -RouteMetric 1 -PolicyStore ActiveStore | Out-Null
"#
        );
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .output()
            .expect("failed to run PowerShell to install Windows e2e host route");

        assert!(
            output.status.success(),
            "failed to install Windows e2e host route {prefix} on {interface_alias}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            prefix,
            interface_alias,
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsHostRoute {
    fn drop(&mut self) {
        let script = format!(
            r#"
Remove-NetRoute -DestinationPrefix "{}" -InterfaceAlias "{}" -Confirm:$false -ErrorAction SilentlyContinue
"#,
            self.prefix, self.interface_alias
        );
        let _ = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .status();
    }
}

#[cfg(target_os = "macos")]
pub struct MacHostRoute {
    ip: String,
    interface_name: String,
}

#[cfg(target_os = "macos")]
impl MacHostRoute {
    pub fn install(ip: Ipv4Addr, interface_name: &str) -> Self {
        let ip = ip.to_string();
        let output = std::process::Command::new("route")
            .args(["-n", "add", "-host", &ip, "-interface", interface_name])
            .output()
            .expect("failed to run route to install macOS e2e host route");

        assert!(
            output.status.success(),
            "failed to install macOS e2e host route {ip}/32 on {interface_name}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            ip,
            interface_name: interface_name.to_string(),
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacHostRoute {
    fn drop(&mut self) {
        let _ = std::process::Command::new("route")
            .args([
                "-n",
                "delete",
                "-host",
                &self.ip,
                "-interface",
                &self.interface_name,
            ])
            .status();
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxHostRoute {
    prefix: String,
    iface: &'static str,
}

#[cfg(target_os = "linux")]
impl LinuxHostRoute {
    pub fn install(ip: Ipv4Addr, iface: &'static str) -> Self {
        let prefix = format!("{ip}/32");
        let output = std::process::Command::new("ip")
            .args(["route", "replace", &prefix, "dev", iface])
            .output()
            .expect("failed to run ip to install Linux e2e host route");

        assert!(
            output.status.success(),
            "failed to install Linux e2e host route {prefix} on {iface}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self { prefix, iface }
    }
}

#[cfg(target_os = "linux")]
impl Drop for LinuxHostRoute {
    fn drop(&mut self) {
        let _ = std::process::Command::new("ip")
            .args(["route", "del", &self.prefix, "dev", self.iface])
            .status();
    }
}

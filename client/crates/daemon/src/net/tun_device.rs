//! TUN device handling (cross-platform).
//!
//! Uses the `tun` crate. Provides a simple async interface returning L3 packets.
//!
//! Responsibilities:
//!  - create and configure a TUN interface on macOS / Linux / Windows,
//!    assigning the gateway IPv4 address and netmask for the virtual subnet,
//!  - ensure the OS routes the whole virtual subnet (`10.254.0.0/16`) into the
//!    interface,
//!  - tear down any route the daemon explicitly added when the device is
//!    dropped.
//!
//! Creating a TUN device and modifying the routing table both require
//! root / `CAP_NET_ADMIN` (or the appropriate entitlement on macOS / Windows).
//! Route management is therefore *best-effort and non-fatal*: a missing route is
//! logged but does not abort startup, mirroring how the rest of the overlay
//! treats privileged operations.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tun::{create_as_async, AsyncDevice, Configuration, Device};

/// Prefix length of the virtual subnet (10.254.0.0/16).
const VNET_PREFIX_LEN: u8 = 16;

/// Length of the macOS `utun` address-family header prepended to every L3
/// packet on read and required on every write.
#[cfg(target_os = "macos")]
const UTUN_AF_HEADER_LEN: usize = 4;

/// macOS `utun` 4-byte big-endian protocol family header for IPv4
/// (`AF_INET` = 2).
#[cfg(target_os = "macos")]
const UTUN_AF_INET: [u8; 4] = [0x00, 0x00, 0x00, 0x02];

/// macOS `utun` 4-byte big-endian protocol family header for IPv6
/// (`AF_INET6` = 0x1E = 30).
#[cfg(target_os = "macos")]
const UTUN_AF_INET6: [u8; 4] = [0x00, 0x00, 0x00, 0x1E];

/// Choose the macOS `utun` 4-byte address-family header for an outgoing bare IP
/// packet, based on the IP version nibble (`packet[0] >> 4`): `4` → `AF_INET`,
/// `6` → `AF_INET6`. Anything else (including an empty packet) defaults to
/// `AF_INET`.
#[cfg(target_os = "macos")]
fn af_header_for(packet: &[u8]) -> [u8; 4] {
    match packet.first().map(|b| b >> 4) {
        Some(6) => UTUN_AF_INET6,
        _ => UTUN_AF_INET,
    }
}

/// Strip the macOS `utun` 4-byte address-family header from a packet just read
/// into `buf` (where `n` is the number of bytes read), shifting the bare IP
/// packet to the front and returning its length. A short read (`n < 4`) yields
/// an empty packet (returns 0) rather than panicking.
///
/// On non-macOS platforms there is no header: this is a byte-for-byte
/// passthrough that returns `n` unchanged.
fn strip_utun_header(buf: &mut [u8], n: usize) -> usize {
    #[cfg(target_os = "macos")]
    {
        if n < UTUN_AF_HEADER_LEN {
            return 0;
        }
        buf.copy_within(UTUN_AF_HEADER_LEN..n, 0);
        n - UTUN_AF_HEADER_LEN
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = buf;
        n
    }
}

/// Configuration for the virtual TUN interface.
#[derive(Debug, Clone)]
pub struct TunConfig {
    /// Suggested device name (platform dependent). `None` lets each platform
    /// pick a sensible default (see [`default_device_name`]).
    ///
    /// macOS: leave `None` for kernel assignment — the `tun` crate then passes
    /// utun unit `id = 0`, letting the kernel pick the next free `utunN`.
    /// Passing a name here on macOS requests a *specific* `utunN` (e.g.
    /// `"utun7"`); the bare prefix `"utun"` is invalid (empty unit number).
    /// Linux: `deven0` (or any `tunN`-style name).
    /// Windows: the wintun adapter name.
    pub name: Option<String>,
    /// IPv4 address to assign to the interface (our side).
    /// This is typically the gateway 10.254.0.1 .
    pub address: Ipv4Addr,
    /// Netmask.
    pub netmask: Ipv4Addr,
    /// MTU. 1500 is safe.
    pub mtu: u32,
}

impl Default for TunConfig {
    fn default() -> Self {
        Self {
            // `None` => let the platform choose (kernel-assigned utunN on macOS,
            // `deven0` on Linux/Windows). Do not hardcode a specific unit number
            // since it may already be in use.
            name: None,
            address: crate::net::virtual_ip::gateway_ip(),
            netmask: Ipv4Addr::new(255, 255, 0, 0),
            mtu: 1500,
        }
    }
}

/// Pick a sensible default interface name for the current platform when the
/// caller did not request one.
///
/// On macOS the kernel assigns the utun unit number itself, so the default path
/// does NOT pass this name to the `tun` crate at all (it leaves the name `None`
/// so the crate uses unit `id = 0` / next-free `utunN`). The bare `"utun"`
/// string returned here is only used as a logging placeholder. On Linux and
/// Windows we use this stable, descriptive name as the real device name.
fn default_device_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "utun"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "deven0"
    }
}

/// Outcome of [`select_device_name`]: which name (if any) to set on the `tun`
/// `Configuration`, plus a non-empty placeholder used only for logging when the
/// post-creation kernel name query fails.
#[derive(Debug, PartialEq, Eq)]
struct DeviceNameSelection {
    /// `Some(name)` => call `tuncfg.name(name)`. `None` => leave the name unset
    /// so the kernel assigns it (macOS default path: next free `utunN`).
    set_name: Option<String>,
    /// Placeholder name for logging if `dev.get_ref().name()` ever fails.
    requested_name: String,
}

/// Decide whether to request a specific device name from the kernel.
///
/// On macOS the `tun` crate maps a name to a utun unit number by parsing the
/// digits after the `utun` prefix; the bare `"utun"` has no digits and fails
/// with "cannot parse integer from empty string". The crate's `None` path
/// instead passes unit `id = 0`, which tells the kernel to assign the next free
/// `utunN`. So on macOS we only set the name when the caller explicitly asked
/// for a specific `utunN` (e.g. `"utun7"`); the default path (`None`) leaves the
/// name unset.
///
/// On Linux/Windows the default (`deven0`) is a real, valid device name and must
/// always be set — behaviour there is unchanged.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
fn select_device_name(requested: Option<&str>) -> DeviceNameSelection {
    #[cfg(target_os = "macos")]
    {
        match requested {
            // Caller asked for a specific utunN — honour it.
            Some(name) => DeviceNameSelection {
                set_name: Some(name.to_string()),
                requested_name: name.to_string(),
            },
            // Default path: leave the name unset so the kernel assigns the next
            // free utunN. The placeholder is used only for logging.
            None => DeviceNameSelection {
                set_name: None,
                requested_name: default_device_name().to_string(),
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux/Windows: always set a concrete, valid name (caller's or the
        // platform default such as `deven0`).
        let name = requested
            .map(|s| s.to_string())
            .unwrap_or_else(|| default_device_name().to_string());
        DeviceNameSelection {
            set_name: Some(name.clone()),
            requested_name: name,
        }
    }
}

/// The virtual subnet in CIDR form (e.g. `10.254.0.0/16`), derived from the
/// gateway address and the subnet prefix length. Pure — used for route commands.
fn subnet_cidr(gateway: Ipv4Addr, prefix_len: u8) -> String {
    let o = gateway.octets();
    // Zero out the host bits below the prefix. For /16 this yields the
    // network address 10.254.0.0.
    let mask: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len as u32)
    };
    let net = u32::from_be_bytes(o) & mask;
    let net = Ipv4Addr::from(net.to_be_bytes());
    format!("{net}/{prefix_len}")
}

/// Generate the OS command + arguments to add a route for `cidr` via the named
/// interface. Returns `None` where the kernel installs the connected route
/// automatically (Windows). Linux adds the route in-process via
/// [`add_route_linux`], so this function is not compiled there.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
#[cfg(not(target_os = "linux"))]
fn route_add_command(cidr: &str, iface: &str) -> Option<(&'static str, Vec<String>)> {
    #[cfg(target_os = "macos")]
    {
        Some((
            "route",
            vec![
                "-n".into(),
                "add".into(),
                "-net".into(),
                cidr.into(),
                "-interface".into(),
                iface.into(),
            ],
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows: the wintun adapter's assigned address installs the connected
        // route, so no explicit command is issued here.
        let _ = (cidr, iface);
        None
    }
}

/// Add a host route for `cidr` (e.g. `10.254.0.0/16`) via `iface` using the
/// `SIOCADDRT` ioctl directly — no subprocess, so the caller's `CAP_NET_ADMIN`
/// file capability is used without the inheritance problem that plagues
/// `ip route add` child processes.
///
/// Returns `true` on success or if the route already exists (idempotent).
#[cfg(target_os = "linux")]
fn add_route_linux(cidr: &str, iface: &str) -> bool {
    use std::ffi::CString;
    use std::mem::zeroed;

    // Parse "x.x.x.x/prefix"
    let Some((addr_str, prefix_str)) = cidr.split_once('/') else {
        tracing::warn!("add_route_linux: malformed cidr {cidr}");
        return false;
    };
    let addr: std::net::Ipv4Addr = match addr_str.parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("add_route_linux: bad address in {cidr}: {e}");
            return false;
        }
    };
    let prefix_len: u8 = match prefix_str.parse() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("add_route_linux: bad prefix in {cidr}: {e}");
            return false;
        }
    };

    let net_be = u32::from_be_bytes(addr.octets()).to_be(); // already host order → to_be = network order
    let mask_host: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len as u32)
    };
    let mask_be = mask_host.to_be();

    let Ok(ifname) = CString::new(iface) else {
        return false;
    };

    unsafe {
        let mut dst: libc::sockaddr_in = zeroed();
        dst.sin_family = libc::AF_INET as libc::sa_family_t;
        dst.sin_addr.s_addr = net_be;

        let mut genmask: libc::sockaddr_in = zeroed();
        genmask.sin_family = libc::AF_INET as libc::sa_family_t;
        genmask.sin_addr.s_addr = mask_be;

        let mut rt: libc::rtentry = zeroed();
        rt.rt_dst = *(&dst as *const libc::sockaddr_in as *const libc::sockaddr);
        rt.rt_genmask = *(&genmask as *const libc::sockaddr_in as *const libc::sockaddr);
        rt.rt_flags = libc::RTF_UP as libc::c_ushort;
        rt.rt_dev = ifname.as_ptr() as *mut libc::c_char;

        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            tracing::warn!(
                "add_route_linux: socket(): {}",
                std::io::Error::last_os_error()
            );
            return false;
        }

        let ret = libc::ioctl(sock, libc::SIOCADDRT, &rt as *const libc::rtentry);
        libc::close(sock);

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EEXIST) {
                tracing::info!("route {cidr} dev {iface} already present (treating as success)");
                return true;
            }
            tracing::warn!("add_route_linux SIOCADDRT {cidr} dev {iface}: {err}");
            return false;
        }

        tracing::info!("route add {cidr} dev {iface} (in-process SIOCADDRT)");
        true
    }
}

/// Generate the OS command + arguments to delete the route previously added by
/// [`route_add_command`]. Mirrors it platform-for-platform.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
#[cfg(not(target_os = "linux"))]
fn route_del_command(cidr: &str, iface: &str) -> Option<(&'static str, Vec<String>)> {
    #[cfg(target_os = "macos")]
    {
        Some((
            "route",
            vec![
                "-n".into(),
                "delete".into(),
                "-net".into(),
                cidr.into(),
                "-interface".into(),
                iface.into(),
            ],
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (cidr, iface);
        None
    }
}

#[cfg(not(target_os = "linux"))]
/// True if a failed `route add` stderr indicates the route is already present
/// (Linux `ip route add` -> "File exists" / `EEXIST`; macOS `route add` ->
/// "File exists" on the routing socket). In that case the route we wanted is
/// already installed, so the add should be treated as success — this keeps
/// reruns clean after a previous run was SIGKILL'd before teardown.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
fn route_add_already_exists(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("file exists")
}

/// True if a failed `TunDevice::create` error indicates the same-named device
/// already exists / is busy. On Linux a leftover `deven0` from an unclean exit
/// (SIGKILL, panic, OOM, power loss) makes `create_as_async` fail with `EBUSY`
/// ("Device or resource busy" / "os error 16"); some paths surface `EEXIST`
/// ("File exists"). In those cases we may delete OUR stale device and retry.
///
/// Pure and side-effect free so it can be unit-tested unprivileged.
fn device_busy_or_exists(err: &str) -> bool {
    let s = err.to_ascii_lowercase();
    s.contains("device or resource busy") || s.contains("os error 16") || s.contains("file exists")
}

/// Best-effort deletion of a stale TUN interface so a fresh one can be created.
/// On Linux we use the `TUNSETPERSIST 0` ioctl directly — no subprocess, so our
/// process's `CAP_NET_ADMIN` capability applies. On other platforms this is a
/// no-op (macOS utun numbers are kernel-assigned; Windows wintun has its own
/// adapter lifecycle).
fn delete_device(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        match delete_tun_linux(name) {
            Ok(()) => {
                tracing::info!("deleted stale TUN device {name} via TUNSETPERSIST ioctl");
                true
            }
            Err(e) => {
                tracing::warn!("could not delete stale TUN device {name}: {e}");
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        false
    }
}

/// Open `/dev/net/tun`, attach to the named interface, then call
/// `TUNSETPERSIST 0` to mark it non-persistent. The device is deleted by the
/// kernel when the file descriptor is closed (at the end of this function).
/// Requires `CAP_NET_ADMIN` on the calling process.
#[cfg(target_os = "linux")]
fn delete_tun_linux(name: &str) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    // ioctl request codes for the Linux TUN driver.
    const TUNSETIFF: libc::c_ulong = 0x400454ca;
    const TUNSETPERSIST: libc::c_ulong = 0x400454cb;
    const IFF_TUN: libc::c_short = 0x0001;
    const IFF_NO_PI: libc::c_short = 0x1000;

    #[repr(C)]
    struct Ifreq {
        ifr_name: [libc::c_char; libc::IFNAMSIZ],
        ifr_flags: libc::c_short,
        _pad: [u8; 22],
    }

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")?;
    let fd = file.as_raw_fd();

    let mut req: Ifreq = unsafe { std::mem::zeroed() };
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
    for (i, &b) in name_bytes[..copy_len].iter().enumerate() {
        req.ifr_name[i] = b as libc::c_char;
    }
    req.ifr_flags = IFF_TUN | IFF_NO_PI;

    // Attach to the existing TUN interface by name.
    let ret = unsafe { libc::ioctl(fd, TUNSETIFF, &req as *const _ as *mut libc::c_void) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Mark non-persistent: kernel deletes the device when the last FD closes.
    let ret = unsafe { libc::ioctl(fd, TUNSETPERSIST, 0i32) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // `file` is dropped here, closing the fd and triggering kernel cleanup.
    Ok(())
}

/// Best-effort: run a route command, logging the outcome. Never returns an
/// error — route management is non-fatal.
#[cfg(not(target_os = "linux"))]
fn run_route_command(action: &str, program: &str, args: &[String]) -> bool {
    match std::process::Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => {
            tracing::info!("route {action} succeeded: {program} {}", args.join(" "));
            true
        }
        // An "already exists" result on add means the route is present (e.g. a
        // leftover from a previous run killed before teardown). Treat as success
        // so the route is still recorded/usable and reruns are idempotent.
        Ok(out)
            if action == "add"
                && route_add_already_exists(&String::from_utf8_lossy(&out.stderr)) =>
        {
            tracing::info!(
                "route add already present (treated as success): {program} {}",
                args.join(" ")
            );
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(
                "route {action} failed ({program} {}): {}",
                args.join(" "),
                stderr.trim()
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                "route {action} could not run ({program} {}): {e}",
                args.join(" ")
            );
            false
        }
    }
}

/// A handle to an async TUN device.
pub struct TunDevice {
    /// `Option` so the device can be moved out in [`TunDevice::split`] despite
    /// the `Drop` impl. Always `Some` until split/drop.
    dev: Option<AsyncDevice>,
    name: String,
    /// The subnet CIDR routed into this interface, set when the daemon explicitly
    /// added an OS route that it is responsible for removing on teardown.
    added_route: Option<String>,
}

impl TunDevice {
    /// Create and configure the TUN device, assign its IPv4 address/netmask, and
    /// ensure the virtual subnet is routed into it.
    ///
    /// On macOS this will typically create a `utunN` interface (kernel-chosen).
    /// Requires root / appropriate entitlements on macOS and Windows, and
    /// `CAP_NET_ADMIN`/root on Linux.
    pub fn create(config: &TunConfig) -> Result<Self> {
        let mut tuncfg = Configuration::default();

        // Decide whether to request a specific device name from the kernel, and
        // what placeholder name to use for logging if the post-creation name
        // query fails. See [`select_device_name`] for the platform rationale.
        let DeviceNameSelection {
            set_name,
            requested_name,
        } = select_device_name(config.name.as_deref());
        if let Some(name) = set_name.as_ref() {
            tuncfg.name(name);
        }

        tuncfg.address(config.address);
        tuncfg.netmask(config.netmask);
        tuncfg.mtu(config.mtu.try_into().unwrap_or(1500));
        tuncfg.up();

        // Layer 3 (we want raw IP packets, not ethernet frames).
        tuncfg.layer(tun::Layer::L3);

        let dev = match create_as_async(&tuncfg) {
            Ok(dev) => dev,
            Err(first_err) => {
                // A leftover same-named device from an unclean exit makes create
                // fail with EBUSY/EEXIST. Best-effort: delete OUR stale device
                // (only the exact name we requested — never an unrelated iface)
                // and retry create exactly ONCE. macOS `utun` units are
                // kernel-assigned so delete_device is a no-op there and we skip
                // straight to surfacing the error.
                let msg = first_err.to_string();
                if device_busy_or_exists(&msg) && cfg!(target_os = "linux") {
                    tracing::warn!(
                        "TUN device {requested_name} appears stale/busy ({}); deleting and retrying once",
                        msg.trim()
                    );
                    delete_device(&requested_name);
                    match create_as_async(&tuncfg) {
                        Ok(dev) => {
                            tracing::info!(
                                "recreated TUN device {requested_name} after removing a stale instance"
                            );
                            dev
                        }
                        // Retry also failed: degrade exactly as before by
                        // returning the ORIGINAL error.
                        Err(_) => {
                            return Err(first_err).with_context(|| {
                                format!(
                                    "failed to create TUN device {requested_name} at {}/{} (are you root?)",
                                    config.address, config.netmask
                                )
                            });
                        }
                    }
                } else {
                    return Err(first_err).with_context(|| {
                        format!(
                            "failed to create TUN device {requested_name} at {}/{} (are you root?)",
                            config.address, config.netmask
                        )
                    });
                }
            }
        };

        // Ask the device what its actual name is (the kernel may have chosen a
        // different unit number, e.g. utun7 instead of the requested utun).
        let actual_name = dev
            .get_ref()
            .name()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| requested_name.clone());

        tracing::info!(
            "TUN device created: name={} address={}/{} mtu={}",
            actual_name,
            config.address,
            VNET_PREFIX_LEN,
            config.mtu
        );

        // Ensure the whole virtual subnet routes into this interface. On most
        // platforms assigning the /16 address already installs a connected
        // route; we additionally add an explicit route where the platform needs
        // it so the subnet is reliably reachable.
        // On Linux we use an in-process ioctl (no subprocess, so file
        // capabilities work). On macOS we shell out to `route`. Best-effort.
        let cidr = subnet_cidr(config.address, VNET_PREFIX_LEN);
        let mut added_route = None;

        #[cfg(target_os = "linux")]
        {
            if add_route_linux(&cidr, &actual_name) {
                added_route = Some(cidr.clone());
            }
        }

        #[cfg(not(target_os = "linux"))]
        if let Some((program, args)) = route_add_command(&cidr, &actual_name) {
            if run_route_command("add", program, &args) {
                added_route = Some(cidr.clone());
            }
        }

        Ok(Self {
            dev: Some(dev),
            name: actual_name,
            added_route,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Read one L3 packet from the TUN.
    /// Returns the number of bytes of the bare IP packet (macOS: with the 4-byte
    /// utun address-family header stripped; other platforms: unchanged).
    pub async fn read_packet(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self
            .dev
            .as_mut()
            .expect("TUN device used after split")
            .read(buf)
            .await?;
        Ok(strip_utun_header(buf, n))
    }

    /// Write one L3 packet to the TUN (macOS: prepending the 4-byte utun
    /// address-family header; other platforms: unchanged).
    pub async fn write_packet(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let dev = self.dev.as_mut().expect("TUN device used after split");
        #[cfg(target_os = "macos")]
        {
            if buf.is_empty() {
                return Ok(());
            }
            let mut framed = Vec::with_capacity(UTUN_AF_HEADER_LEN + buf.len());
            framed.extend_from_slice(&af_header_for(buf));
            framed.extend_from_slice(buf);
            dev.write_all(&framed).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            dev.write_all(buf).await
        }
    }

    /// Split into read and write halves for concurrent use.
    ///
    /// This consumes the [`TunDevice`]. Ownership of the explicitly-added route
    /// is transferred into the [`TunWriter`] half, which removes it on drop so
    /// teardown still happens after a split.
    pub fn split(mut self) -> (TunReader, TunWriter) {
        // Move the route bookkeeping out before the device is consumed so the
        // `Drop` impl on `self` does not run teardown prematurely.
        let added_route = self.added_route.take();
        let name = self.name.clone();
        let dev = self
            .dev
            .take()
            .expect("TUN device already split or dropped");
        let (r, w) = tokio::io::split(dev);
        (
            TunReader {
                inner: Arc::new(tokio::sync::Mutex::new(r)),
                _name: name.clone(),
            },
            TunWriter {
                inner: Arc::new(tokio::sync::Mutex::new(w)),
                route_guard: RouteGuard { name, added_route },
            },
        )
    }
}

impl Drop for TunDevice {
    fn drop(&mut self) {
        remove_added_route(&self.name, self.added_route.take());
    }
}

/// Owns the responsibility of removing the explicitly-added OS route. Held by
/// the [`TunWriter`] after a [`TunDevice::split`] so teardown happens exactly
/// once (the writer outlives the route just like the device did).
struct RouteGuard {
    name: String,
    added_route: Option<String>,
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        remove_added_route(&self.name, self.added_route.take());
    }
}

/// Delete the route for `cidr` via `iface` in-process using `SIOCDELRT`.
/// Mirrors `add_route_linux`; see that function for rationale.
#[cfg(target_os = "linux")]
fn del_route_linux(cidr: &str, iface: &str) {
    use std::ffi::CString;
    use std::mem::zeroed;

    let Some((addr_str, prefix_str)) = cidr.split_once('/') else {
        return;
    };
    let Ok(addr) = addr_str.parse::<std::net::Ipv4Addr>() else {
        return;
    };
    let Ok(prefix_len) = prefix_str.parse::<u8>() else {
        return;
    };

    let net_be = u32::from_be_bytes(addr.octets()).to_be();
    let mask_host: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len as u32)
    };
    let mask_be = mask_host.to_be();
    let Ok(ifname) = CString::new(iface) else {
        return;
    };

    unsafe {
        let mut dst: libc::sockaddr_in = zeroed();
        dst.sin_family = libc::AF_INET as libc::sa_family_t;
        dst.sin_addr.s_addr = net_be;

        let mut genmask: libc::sockaddr_in = zeroed();
        genmask.sin_family = libc::AF_INET as libc::sa_family_t;
        genmask.sin_addr.s_addr = mask_be;

        let mut rt: libc::rtentry = zeroed();
        rt.rt_dst = *(&dst as *const libc::sockaddr_in as *const libc::sockaddr);
        rt.rt_genmask = *(&genmask as *const libc::sockaddr_in as *const libc::sockaddr);
        rt.rt_flags = libc::RTF_UP as libc::c_ushort;
        rt.rt_dev = ifname.as_ptr() as *mut libc::c_char;

        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return;
        }
        let ret = libc::ioctl(sock, libc::SIOCDELRT, &rt as *const libc::rtentry);
        libc::close(sock);

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                // ESRCH = no such route (already gone); anything else is worth noting.
                tracing::warn!("del_route_linux SIOCDELRT {cidr} dev {iface}: {err}");
            }
        } else {
            tracing::info!("route del {cidr} dev {iface} (in-process SIOCDELRT)");
        }
    }
}

/// Best-effort removal of a route the daemon previously installed. Routes the
/// kernel auto-installed for the connected /16 vanish with the interface, so we
/// only ever remove what we explicitly added.
fn remove_added_route(iface: &str, added_route: Option<String>) {
    let Some(cidr) = added_route else {
        return;
    };

    #[cfg(target_os = "linux")]
    {
        del_route_linux(&cidr, iface);
    }

    #[cfg(not(target_os = "linux"))]
    if let Some((program, args)) = route_del_command(&cidr, iface) {
        run_route_command("del", program, &args);
    }
}

pub struct TunReader {
    inner: Arc<tokio::sync::Mutex<tokio::io::ReadHalf<AsyncDevice>>>,
    _name: String,
}

impl TunReader {
    /// Read one bare IP packet. On macOS the leading 4-byte utun
    /// address-family header is stripped before returning; on other platforms
    /// the bytes are returned unchanged.
    pub async fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut guard = self.inner.lock().await;
        let n = guard.read(buf).await?;
        Ok(strip_utun_header(buf, n))
    }
}

pub struct TunWriter {
    inner: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<AsyncDevice>>>,
    /// Removes the daemon-installed route when this writer (the last surviving
    /// handle to the device after a split) is dropped.
    #[allow(dead_code)]
    route_guard: RouteGuard,
}

impl TunWriter {
    /// Write one bare IP packet. On macOS the 4-byte utun address-family header
    /// (chosen from the IP version nibble) is prepended; on other platforms the
    /// bytes are written unchanged.
    pub async fn write(&self, buf: &[u8]) -> std::io::Result<()> {
        let mut guard = self.inner.lock().await;
        #[cfg(target_os = "macos")]
        {
            if buf.is_empty() {
                return Ok(());
            }
            let mut framed = Vec::with_capacity(UTUN_AF_HEADER_LEN + buf.len());
            framed.extend_from_slice(&af_header_for(buf));
            framed.extend_from_slice(buf);
            guard.write_all(&framed).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            guard.write_all(buf).await
        }
    }
}

// Convenience: platform specific notes for privileged setup.
#[cfg(target_os = "macos")]
pub fn platform_setup_hints() {
    tracing::info!("macOS: ensure the binary runs as root or has the com.apple.developer.networking.networkextension entitlement for System Extension utun.");
}

#[cfg(target_os = "linux")]
pub fn platform_setup_hints() {
    tracing::info!("Linux: the daemon must have CAP_NET_ADMIN or run as root to create /dev/net/tun devices and modify routes.");
}

#[cfg(target_os = "windows")]
pub fn platform_setup_hints() {
    tracing::info!("Windows: wintun.dll must be present next to the binary or in PATH.");
}

#[cfg(test)]
#[path = "tun_device_tests.rs"]
mod tests;

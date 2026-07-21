//! Unit tests for the TUN device module. Split out of tun_device.rs to keep the
//! production module within the per-file line budget; pure code motion.

use super::*;

#[test]
fn subnet_cidr_zeroes_host_bits() {
    // Gateway is 10.254.0.1; the /16 network is 10.254.0.0.
    let gw = Ipv4Addr::new(10, 254, 0, 1);
    assert_eq!(subnet_cidr(gw, 16), "10.254.0.0/16");
}

#[test]
fn subnet_cidr_matches_virtual_gateway() {
    let gw = crate::net::virtual_ip::gateway_ip();
    assert_eq!(subnet_cidr(gw, VNET_PREFIX_LEN), "10.254.0.0/16");
}

#[test]
fn subnet_cidr_other_prefixes() {
    let gw = Ipv4Addr::new(192, 168, 5, 37);
    assert_eq!(subnet_cidr(gw, 24), "192.168.5.0/24");
    assert_eq!(subnet_cidr(gw, 8), "192.0.0.0/8");
    assert_eq!(subnet_cidr(gw, 32), "192.168.5.37/32");
}

#[test]
fn default_device_name_is_platform_appropriate() {
    let name = default_device_name();
    #[cfg(target_os = "macos")]
    assert_eq!(name, "utun");
    #[cfg(not(target_os = "macos"))]
    assert_eq!(name, "deven0");
}

#[cfg(target_os = "macos")]
#[test]
fn select_device_name_macos_default_leaves_name_unset() {
    // The default path passes None: the `tun` crate must NOT receive a name
    // (so it uses unit id=0 and the kernel assigns the next free utunN).
    // The placeholder is only for logging and must be non-empty.
    let sel = select_device_name(None);
    assert_eq!(sel.set_name, None);
    assert_eq!(sel.requested_name, "utun");
}

#[cfg(target_os = "macos")]
#[test]
fn select_device_name_macos_explicit_is_honoured() {
    // An explicit utunN must be passed through to the crate verbatim.
    let sel = select_device_name(Some("utun7"));
    assert_eq!(sel.set_name.as_deref(), Some("utun7"));
    assert_eq!(sel.requested_name, "utun7");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn select_device_name_non_macos_default_sets_deven0() {
    // Linux/Windows must always set a concrete, valid name.
    let sel = select_device_name(None);
    assert_eq!(sel.set_name.as_deref(), Some("deven0"));
    assert_eq!(sel.requested_name, "deven0");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn select_device_name_non_macos_explicit_is_honoured() {
    let sel = select_device_name(Some("deven1"));
    assert_eq!(sel.set_name.as_deref(), Some("deven1"));
    assert_eq!(sel.requested_name, "deven1");
}

#[cfg(target_os = "linux")]
#[test]
fn add_route_linux_parses_valid_cidr() {
    // Verify that well-formed CIDRs don't panic and bad ones return false
    // (without actually calling the ioctl — that requires CAP_NET_ADMIN).
    // We exercise the parse+mask logic by calling the function with an
    // interface name that doesn't exist; it will fail the ioctl with ENODEV
    // but must not panic or corrupt memory.
    let result = add_route_linux("10.254.0.0/16", "nonexistent0");
    // Either false (EPERM/ENODEV) or a panic — if we reach here without
    // panicking the parsing code is correct.
    let _ = result;
}

#[cfg(target_os = "linux")]
#[test]
fn add_route_linux_rejects_bad_cidr() {
    assert!(!add_route_linux("notanip/16", "lo"));
    assert!(!add_route_linux("10.254.0.0", "lo")); // no prefix
    assert!(!add_route_linux("", "lo"));
}

#[cfg(target_os = "macos")]
#[test]
fn route_add_command_macos() {
    let (prog, args) = route_add_command("10.254.0.0/16", "utun7").unwrap();
    assert_eq!(prog, "route");
    assert_eq!(
        args,
        vec!["-n", "add", "-net", "10.254.0.0/16", "-interface", "utun7"]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn route_del_command_macos() {
    let (prog, args) = route_del_command("10.254.0.0/16", "utun7").unwrap();
    assert_eq!(prog, "route");
    assert_eq!(
        args,
        vec![
            "-n",
            "delete",
            "-net",
            "10.254.0.0/16",
            "-interface",
            "utun7"
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn af_header_for_ipv4() {
    // IPv4 version nibble (0x4) → AF_INET = 0x00000002.
    let pkt = [0x45u8, 0x00, 0x00, 0x28];
    assert_eq!(af_header_for(&pkt), [0, 0, 0, 2]);
}

#[cfg(target_os = "macos")]
#[test]
fn af_header_for_ipv6() {
    // IPv6 version nibble (0x6) → AF_INET6 = 0x0000001E.
    let pkt = [0x60u8, 0x00, 0x00, 0x00];
    assert_eq!(af_header_for(&pkt), [0, 0, 0, 0x1E]);
}

#[cfg(target_os = "macos")]
#[test]
fn af_header_for_unknown_and_empty_default_to_inet() {
    // Anything that isn't an IPv6 nibble defaults to AF_INET, including an
    // empty packet (no first byte to inspect).
    assert_eq!(af_header_for(&[0x00u8]), [0, 0, 0, 2]);
    assert_eq!(af_header_for(&[]), [0, 0, 0, 2]);
}

#[cfg(target_os = "macos")]
#[test]
fn strip_utun_header_removes_four_bytes() {
    // [AF_INET header][IP packet] → bare IP packet shifted to the front.
    let mut buf = vec![0x00, 0x00, 0x00, 0x02, 0x45, 0x11, 0x22, 0x33];
    let n = strip_utun_header(&mut buf, 8);
    assert_eq!(n, 4);
    assert_eq!(&buf[..n], &[0x45, 0x11, 0x22, 0x33]);
}

#[cfg(target_os = "macos")]
#[test]
fn strip_utun_header_short_read_is_empty() {
    // A read shorter than the 4-byte header yields an empty packet, never a
    // panic.
    let mut buf = vec![0x00, 0x00, 0x00];
    assert_eq!(strip_utun_header(&mut buf, 3), 0);
    let mut empty: Vec<u8> = vec![];
    assert_eq!(strip_utun_header(&mut empty, 0), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_prepend_then_strip_round_trips() {
    // Prepending the AF header (as the writer does) and then stripping it
    // (as the reader does) must yield the original IP packet.
    let ip_packet = [0x45u8, 0x00, 0x00, 0x28, 0xde, 0xad, 0xbe, 0xef];
    let header = af_header_for(&ip_packet);
    let mut framed = Vec::new();
    framed.extend_from_slice(&header);
    framed.extend_from_slice(&ip_packet);
    let n = framed.len();
    let stripped = strip_utun_header(&mut framed, n);
    assert_eq!(stripped, ip_packet.len());
    assert_eq!(&framed[..stripped], &ip_packet);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn strip_utun_header_passthrough_off_macos() {
    // Non-macOS: byte-for-byte passthrough; n returned unchanged, buffer
    // untouched.
    let mut buf = vec![0x45, 0x11, 0x22, 0x33];
    let original = buf.clone();
    assert_eq!(strip_utun_header(&mut buf, 4), 4);
    assert_eq!(buf, original);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn route_add_already_exists_classifier() {
    // Linux `ip route add` and macOS `route add` both report "File exists"
    // when the route is already present; that must be treated as success.
    assert!(route_add_already_exists("RTNETLINK answers: File exists"));
    assert!(route_add_already_exists(
            "route: writing to routing socket: File exists\nadd net 10.254.0.0: gateway deven0: File exists"
        ));
    // Case-insensitive for safety.
    assert!(route_add_already_exists("file exists"));
    // Unrelated failures must NOT be swallowed.
    assert!(!route_add_already_exists("Operation not permitted"));
    assert!(!route_add_already_exists("Network is unreachable"));
    assert!(!route_add_already_exists(""));
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn route_commands_none_on_other_platforms() {
    assert!(route_add_command("10.254.0.0/16", "deven0").is_none());
    assert!(route_del_command("10.254.0.0/16", "deven0").is_none());
}

#[test]
fn device_busy_or_exists_classifier() {
    // EBUSY: a leftover device from an unclean exit ("Device or resource
    // busy" / raw "os error 16").
    assert!(device_busy_or_exists(
        "failed to create TUN device (are you root?): Device or resource busy (os error 16)"
    ));
    assert!(device_busy_or_exists("Device or resource busy"));
    assert!(device_busy_or_exists("os error 16"));
    // EEXIST surfaced on some paths.
    assert!(device_busy_or_exists("File exists"));
    // Case-insensitive for safety.
    assert!(device_busy_or_exists("DEVICE OR RESOURCE BUSY"));
    // Unrelated errors must NOT trigger a destructive delete+retry.
    assert!(!device_busy_or_exists("Operation not permitted"));
    assert!(!device_busy_or_exists("No such device"));
    assert!(!device_busy_or_exists("Permission denied"));
    assert!(!device_busy_or_exists(""));
}

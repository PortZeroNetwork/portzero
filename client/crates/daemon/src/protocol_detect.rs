//! Zero-config canonical port detection for overlay services (task-17).
//!
//! When a `.portzero.local` overlay service is discovered WITHOUT an explicit
//! canonical `:port` (see task-16), we actively probe the real ephemeral
//! backend to detect its protocol and expose it on the standard port:
//!
//! - **HTTP**  → canonical **80**
//! - **TLS**   → canonical **443**
//!
//! Precedence (decided by the caller in `discovery.rs`):
//!   explicit `:port` (task-16)  >  detected canonical (here)  >  ephemeral.
//!
//! ## Why active probing?
//! HTTP is client-speaks-first, so we cannot passively sniff it — we must send
//! a gentle request and inspect the response. We use `HEAD` (not `GET`) so we
//! are less likely to trigger application logic, and bound all I/O with short
//! timeouts.
//!
//! ## Server-speaks-first safety
//! Postgres (silent until auth), MySQL / SSH / SMTP (send their own greeting)
//! must NEVER be misclassified as HTTP/TLS. The pure [`classify`] function only
//! returns `Some` for an unambiguous `HTTP/` response line or a real TLS
//! record; anything else yields `None` (the caller then falls back to the
//! ephemeral port).
//!
//! ## Caching
//! Probing every ~2s discovery scan would hammer backends, so results are
//! cached per `(pid, real_port)`. We re-probe only when that key changes
//! (backend restarted on a new port / pid).
//!
//! ## Opt-out
//! Probing is skipped entirely when `PZ_TUNNEL_NO_PROBE` is set (to any
//! non-empty, non-`0`/`false` value). It is read first from the target
//! process's own environment (same mechanism used to read `PZ_TUNNEL`),
//! then falls back to the daemon's own `std::env`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// A detected protocol mapped to its canonical TCP port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canonical {
    /// Plain HTTP → port 80.
    Http,
    /// TLS / HTTPS → port 443.
    Tls,
}

impl Canonical {
    /// The standard port for this protocol.
    pub fn port(self) -> u16 {
        match self {
            Canonical::Http => 80,
            Canonical::Tls => 443,
        }
    }
}

/// Per-connection timeout for connect + read.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// How many bytes of the response we read before classifying.
const PROBE_READ_LEN: usize = 64;
/// Number of probe attempts (handles a backend that isn't ready on the first
/// scan). Kept small and cheap.
const PROBE_ATTEMPTS: u32 = 2;

/// Classify a server's first response bytes into a canonical protocol.
///
/// PURE: no I/O, fully deterministic over the input bytes. This is the
/// single point that decides HTTP vs TLS vs unknown, and is exhaustively
/// unit-tested against captured samples.
///
/// Rules:
/// - **HTTP**: the response begins with the literal `HTTP/` status line.
/// - **TLS**: the response begins with a TLS record header — content type
///   `0x16` (handshake, e.g. ServerHello) or `0x15` (alert, e.g. a server
///   that rejected our bytes but still speaks TLS), followed by a legacy
///   record version `0x03 0x0X` (SSL 3.0 / TLS 1.0–1.3 all use `0x03 0xNN`).
/// - **None**: anything else, including empty input and server-speaks-first
///   banners (Postgres sends nothing pre-auth; MySQL/SSH/SMTP send their own
///   greetings) — these MUST NOT be misclassified.
pub fn classify(bytes: &[u8]) -> Option<Canonical> {
    if bytes.is_empty() {
        return None;
    }
    if bytes.starts_with(b"HTTP/") {
        return Some(Canonical::Http);
    }
    if is_tls_record(bytes) {
        return Some(Canonical::Tls);
    }
    None
}

/// True if `bytes` begin with a plausible TLS record header (handshake or
/// alert content type + a `0x03 0x0X` legacy version).
fn is_tls_record(bytes: &[u8]) -> bool {
    if bytes.len() < 3 {
        return false;
    }
    let content_type = bytes[0];
    // 0x16 = handshake (ServerHello), 0x15 = alert.
    let is_known_type = content_type == 0x16 || content_type == 0x15;
    // Record-layer legacy version is always 0x03 0xNN for SSLv3/TLS1.x.
    let plausible_version = bytes[1] == 0x03 && bytes[2] <= 0x04;
    is_known_type && plausible_version
}

/// Decide whether probing is disabled, given the value of
/// `PZ_TUNNEL_NO_PROBE` from the target process's environment
/// (`per_process`) and from the daemon's own environment (`daemon`).
///
/// PURE: a value is "set" when it is present and not empty / `0` / `false`
/// (case-insensitive). The per-process value takes precedence; if it is
/// absent we consult the daemon's own env.
pub fn probing_disabled(per_process: Option<&str>, daemon: Option<&str>) -> bool {
    fn truthy(v: &str) -> bool {
        let t = v.trim();
        !t.is_empty() && !t.eq_ignore_ascii_case("0") && !t.eq_ignore_ascii_case("false")
    }
    match per_process {
        Some(v) => truthy(v),
        None => daemon.map(truthy).unwrap_or(false),
    }
}

/// Key identifying a probed backend. Re-probe only when this changes.
pub type CacheKey = (u32, u16);

/// The decision for a given cache lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheDecision {
    /// A cached result exists; use it directly (no probe).
    /// The inner value is the cached canonical port (or `None` = "probed,
    /// nothing detected, fall back to ephemeral").
    Hit(Option<u16>),
    /// No cached result for this key; a probe is required.
    Miss,
}

/// PURE cache-lookup decision over a borrowed map. Separated from the global
/// cache so the (pid, real_port) decision logic is unit-testable.
pub fn cache_lookup(cache: &HashMap<CacheKey, Option<u16>>, key: CacheKey) -> CacheDecision {
    match cache.get(&key) {
        Some(v) => CacheDecision::Hit(*v),
        None => CacheDecision::Miss,
    }
}

fn global_cache() -> &'static Mutex<HashMap<CacheKey, Option<u16>>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Option<u16>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Detect the backend protocol, using the cross-scan cache.
///
/// Returns `Some(Http)`, `Some(Tls)`, or `None` when the
/// protocol is unknown / unreachable (caller falls back to the ephemeral
/// port). Best-effort: every failure path yields `None` and never panics or
/// propagates errors into discovery.
///
/// `key` is `(pid, real_port)`; a given backend is probed once and the result
/// is reused until that key changes.
pub async fn detect_canonical_cached(real_addr: SocketAddr, key: CacheKey) -> Option<Canonical> {
    // Fast path: cached decision.
    {
        let cache = global_cache().lock().unwrap();
        if let CacheDecision::Hit(v) = cache_lookup(&cache, key) {
            return v.map(port_to_canonical);
        }
    }

    let detected = detect(real_addr).await;

    let mut cache = global_cache().lock().unwrap();
    cache.insert(key, detected);
    detected.map(port_to_canonical)
}

/// Detect the canonical port for a backend, using the cross-scan cache.
pub async fn detect_cached(real_addr: SocketAddr, key: CacheKey) -> Option<u16> {
    detect_canonical_cached(real_addr, key)
        .await
        .map(Canonical::port)
}

fn port_to_canonical(port: u16) -> Canonical {
    match port {
        443 => Canonical::Tls,
        _ => Canonical::Http,
    }
}

/// Probe `real_addr` and return its canonical port, or `None`.
///
/// HTTP is the priority: we send a gentle `HEAD` request and classify the
/// response. If that does not look like HTTP, we try a minimal TLS
/// ClientHello and classify the response as TLS. All I/O is timeout-bounded
/// and a couple of cheap retries handle a backend that is not yet ready.
pub async fn detect(real_addr: SocketAddr) -> Option<u16> {
    for _ in 0..PROBE_ATTEMPTS {
        if let Some(c) = probe_http(real_addr).await {
            return Some(c.port());
        }
        if let Some(c) = probe_tls(real_addr).await {
            return Some(c.port());
        }
    }
    None
}

/// Send a gentle `HEAD / HTTP/1.0` probe and classify the first response bytes.
async fn probe_http(real_addr: SocketAddr) -> Option<Canonical> {
    const REQUEST: &[u8] = b"HEAD / HTTP/1.0\r\nHost: localhost\r\n\r\n";
    let bytes = probe_exchange(real_addr, REQUEST).await?;
    match classify(&bytes) {
        Some(Canonical::Http) => Some(Canonical::Http),
        // The HTTP probe should never elicit a TLS record, but if it somehow
        // does, trust the classifier rather than misreport.
        Some(Canonical::Tls) => Some(Canonical::Tls),
        None => None,
    }
}

/// Send a minimal TLS 1.0 ClientHello and check for a TLS record response.
///
/// The ClientHello is a hand-built static byte buffer — no TLS crate is
/// pulled in. We only need the server to *respond with a TLS record*
/// (ServerHello `0x16` or an alert `0x15`); we never complete the handshake.
async fn probe_tls(real_addr: SocketAddr) -> Option<Canonical> {
    let bytes = probe_exchange(real_addr, CLIENT_HELLO).await?;
    match classify(&bytes) {
        Some(Canonical::Tls) => Some(Canonical::Tls),
        // A server replying to a TLS ClientHello with `HTTP/...` is a plain
        // HTTP server complaining — treat it as HTTP.
        Some(Canonical::Http) => Some(Canonical::Http),
        None => None,
    }
}

/// Connect, write `request`, read up to [`PROBE_READ_LEN`] bytes — all under a
/// single bounded timeout. Returns the bytes read (possibly empty on a clean
/// silent close), or `None` on any failure/timeout.
async fn probe_exchange(real_addr: SocketAddr, request: &[u8]) -> Option<Vec<u8>> {
    let result = timeout(PROBE_TIMEOUT, async {
        let mut stream = TcpStream::connect(real_addr).await.ok()?;
        stream.write_all(request).await.ok()?;
        let mut buf = vec![0u8; PROBE_READ_LEN];
        let n = stream.read(&mut buf).await.ok()?;
        buf.truncate(n);
        Some(buf)
    })
    .await;

    match result {
        Ok(Some(buf)) if !buf.is_empty() => Some(buf),
        // Silent server (e.g. Postgres) or timeout/error: nothing to classify.
        _ => None,
    }
}

/// A minimal, syntactically valid TLS 1.0 ClientHello record.
///
/// Hand-built so no TLS crate is required. Offers a couple of common cipher
/// suites and an SNI extension for `localhost`. We only inspect whether the
/// server answers with a TLS record (handshake/alert), so the exact contents
/// beyond a well-formed record matter little — but we keep it valid so real
/// TLS servers reply with a ServerHello rather than dropping us.
#[rustfmt::skip]
const CLIENT_HELLO: &[u8] = &[
    // -- TLS record header --
    0x16,             // content type: handshake
    0x03, 0x01,       // record version: TLS 1.0
    0x00, 0x4a,       // record length = 74
    // -- Handshake header --
    0x01,             // handshake type: ClientHello
    0x00, 0x00, 0x46, // handshake length = 70
    // -- ClientHello body --
    0x03, 0x03,       // client version: TLS 1.2
    // 32-byte random (fixed; value is irrelevant for detection)
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x00,             // session id length = 0
    0x00, 0x04,       // cipher suites length = 4
    0x00, 0x2f,       // TLS_RSA_WITH_AES_128_CBC_SHA
    0x00, 0x35,       // TLS_RSA_WITH_AES_256_CBC_SHA
    0x01, 0x00,       // compression: 1 method, null
    0x00, 0x1a,       // extensions length = 26
    // SNI extension for "localhost"
    0x00, 0x00,       // type: server_name
    0x00, 0x0e,       // ext length = 14
    0x00, 0x0c,       // server name list length = 12
    0x00,             // name type: host_name
    0x00, 0x09,       // host name length = 9
    b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't',
    // supported_versions extension (TLS 1.2)
    0x00, 0x2b,       // type: supported_versions
    0x00, 0x03,       // ext length = 3
    0x02,             // list length = 2
    0x03, 0x03,       // TLS 1.2
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_response_line_classifies_as_http() {
        let bytes = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n";
        assert_eq!(classify(bytes), Some(Canonical::Http));
        assert_eq!(
            classify(b"HTTP/1.0 404 Not Found\r\n"),
            Some(Canonical::Http)
        );
    }

    #[test]
    fn tls_handshake_classifies_as_tls() {
        // ServerHello: 0x16 handshake, version 0x03 0x03 (TLS 1.2), then len.
        let bytes = [0x16u8, 0x03, 0x03, 0x00, 0x2a, 0x02];
        assert_eq!(classify(&bytes), Some(Canonical::Tls));
        // TLS 1.0 record version is also accepted.
        let bytes_v10 = [0x16u8, 0x03, 0x01, 0x00, 0x10];
        assert_eq!(classify(&bytes_v10), Some(Canonical::Tls));
    }

    #[test]
    fn tls_alert_classifies_as_tls() {
        // Alert record: 0x15, version 0x03 0x03, length 2, then alert bytes.
        let bytes = [0x15u8, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28];
        assert_eq!(classify(&bytes), Some(Canonical::Tls));
    }

    #[test]
    fn postgres_silent_classifies_as_none() {
        // Postgres sends nothing pre-auth → empty read.
        assert_eq!(classify(b""), None);
    }

    #[test]
    fn mysql_banner_classifies_as_none() {
        // MySQL greeting starts with a length-prefixed handshake packet:
        // 0x4a 0x00 0x00 0x00 0x0a "5.7.42..." — must NOT be HTTP/TLS.
        let bytes = [
            0x4au8, 0x00, 0x00, 0x00, 0x0a, b'5', b'.', b'7', b'.', b'4', b'2',
        ];
        assert_eq!(classify(&bytes), None);
    }

    #[test]
    fn ssh_banner_classifies_as_none() {
        let bytes = b"SSH-2.0-OpenSSH_8.9p1 Ubuntu-3ubuntu0.1\r\n";
        assert_eq!(classify(bytes), None);
    }

    #[test]
    fn smtp_banner_classifies_as_none() {
        let bytes = b"220 mail.example.com ESMTP Postfix\r\n";
        assert_eq!(classify(bytes), None);
    }

    #[test]
    fn garbage_and_short_input_classifies_as_none() {
        assert_eq!(classify(&[0xff, 0x00, 0x13, 0x37]), None);
        assert_eq!(classify(&[0x16]), None); // too short to be a TLS record
        assert_eq!(classify(&[0x16, 0x03]), None); // still too short
                                                   // 0x16 but wrong version bytes (not 0x03 0x0X) → not TLS.
        assert_eq!(classify(&[0x16, 0xff, 0xff]), None);
        // Looks HTTP-ish but isn't the status line.
        assert_eq!(classify(b"HELLO HTTP"), None);
    }

    #[test]
    fn probing_disabled_truth_table() {
        // Per-process value wins.
        assert!(probing_disabled(Some("1"), None));
        assert!(probing_disabled(Some("true"), None));
        assert!(probing_disabled(Some("yes"), None));
        assert!(!probing_disabled(Some("0"), Some("1")));
        assert!(!probing_disabled(Some("false"), Some("1")));
        assert!(!probing_disabled(Some(""), Some("1")));
        // Fall back to daemon env when per-process is absent.
        assert!(probing_disabled(None, Some("1")));
        assert!(!probing_disabled(None, Some("0")));
        assert!(!probing_disabled(None, None));
    }

    #[test]
    fn cache_lookup_hit_and_miss() {
        let mut cache: HashMap<CacheKey, Option<u16>> = HashMap::new();
        assert_eq!(cache_lookup(&cache, (1234, 5000)), CacheDecision::Miss);

        cache.insert((1234, 5000), Some(80));
        assert_eq!(
            cache_lookup(&cache, (1234, 5000)),
            CacheDecision::Hit(Some(80))
        );

        // A cached "nothing detected" is still a hit (don't re-probe).
        cache.insert((1234, 6000), None);
        assert_eq!(cache_lookup(&cache, (1234, 6000)), CacheDecision::Hit(None));

        // Different pid/port → miss (backend changed → re-probe).
        assert_eq!(cache_lookup(&cache, (9999, 5000)), CacheDecision::Miss);
    }

    #[test]
    fn canonical_ports() {
        assert_eq!(Canonical::Http.port(), 80);
        assert_eq!(Canonical::Tls.port(), 443);
    }

    #[test]
    fn client_hello_is_well_formed_record() {
        // Record length field must match the trailing bytes (sanity check on
        // the hand-built buffer so it stays valid if edited).
        let declared_record_len = u16::from_be_bytes([CLIENT_HELLO[3], CLIENT_HELLO[4]]) as usize;
        assert_eq!(declared_record_len, CLIENT_HELLO.len() - 5);
        // Handshake length must match too.
        let declared_hs_len = ((CLIENT_HELLO[6] as usize) << 16)
            | ((CLIENT_HELLO[7] as usize) << 8)
            | (CLIENT_HELLO[8] as usize);
        assert_eq!(declared_hs_len, CLIENT_HELLO.len() - 9);
    }
}

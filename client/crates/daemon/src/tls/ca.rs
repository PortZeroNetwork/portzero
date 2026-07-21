//! Local CA: generates and persists a self-signed CA and wildcard TLS cert
//! for the *.portzero.local zone.
//!
//! On first use (or when the wildcard cert nears expiry), generates:
//!   - A 10-year self-signed CA ("PortZero Local CA")
//!   - A 1-year wildcard cert for *.portzero.local signed by that CA
//!
//! Both are stored as PEM files in the platform data directory:
//!   - macOS:   ~/Library/Application Support/PortZero/
//!   - Linux:   ~/.local/share/PortZero/
//!   - Windows: %APPDATA%\PortZero\
//!
//! The CA cert path is exposed via [`LocalCa::ca_cert_path`] for the trust
//! store installer (see `tls::trust`).  The wildcard cert + key are consumed
//! by the rustls ServerConfig in the overlay stack (see `tls::server_config`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, KeyPair, KeyUsagePurpose, NameConstraints,
};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

const CA_CERT: &str = "ca.crt";
const WILDCARD_CERT: &str = "wildcard.crt";
const WILDCARD_KEY: &str = "wildcard.key";
const WILDCARD_EXPIRY: &str = "wildcard.expiry";

/// DNS subtree the CA is permitted to issue certificates for. Per RFC 5280
/// §4.2.1.10 a DNS permitted subtree of `portzero.local` matches the apex name
/// and every subdomain (`portzero.local`, `*.portzero.local`, `a.b.portzero.local`).
const CA_NAME_CONSTRAINT_DNS: &str = "portzero.local";

const CA_VALIDITY_DAYS: i64 = 365 * 10;
const WILDCARD_VALIDITY_DAYS: i64 = 365;
// Regenerate when fewer than this many days remain on the wildcard cert.
const RENEW_WITHIN_DAYS: i64 = 30;

/// PEM-encoded local CA and wildcard cert material for `*.portzero.local`.
#[derive(Clone)]
pub struct LocalCa {
    /// CA certificate PEM — install into OS trust stores.
    pub ca_cert_pem: String,
    /// Wildcard cert PEM for `*.portzero.local`.
    pub wildcard_cert_pem: String,
    /// Private key PEM for the wildcard cert.
    pub wildcard_key_pem: String,
}

impl LocalCa {
    /// Generate a fresh `LocalCa` in memory without writing anything to disk.
    ///
    /// Useful for tests and one-shot tooling where persistence is not wanted.
    /// For the daemon's normal lifecycle use [`load_or_create`].
    pub fn generate_ephemeral() -> Result<Self> {
        let (ca, _expiry) = generate()?;
        Ok(ca)
    }

    /// Load cert material from disk, or generate and persist fresh material if
    /// the files are absent or within [`RENEW_WITHIN_DAYS`] of expiry.
    pub fn load_or_create() -> Result<Self> {
        let dir = data_dir()?;
        crate::secure_file::ensure_private_dir(&dir)
            .with_context(|| format!("create PortZero data dir: {}", dir.display()))?;

        if is_fresh(&dir) && has_browser_tls_usages(&dir) {
            match load(&dir) {
                Ok(ca) => {
                    if !ca_pem_is_name_constrained(&ca.ca_cert_pem) {
                        // Do NOT auto-regenerate under the daemon: that would silently
                        // break trusted HTTPS until the user re-ran `trust install`.
                        tracing::warn!(
                            "local CA is not name-constrained; a leaked CA key could impersonate any site. Run `portzero trust generate && sudo portzero trust install` to replace it."
                        );
                    }
                    return Ok(ca);
                }
                Err(e) => {
                    tracing::warn!("failed to load existing TLS certs, regenerating: {e:#}");
                }
            }
        }

        tracing::info!("generating PortZero local CA and wildcard cert");
        let (ca, expiry) = generate()?;
        save(&ca, expiry, &dir)?;
        Ok(ca)
    }

    /// Ensure the persisted CA carries the X.509 name-constraints extension,
    /// regenerating it in place if a legacy (unconstrained) CA is found.
    ///
    /// Returns `true` iff an existing CA was replaced because it lacked the
    /// constraint. This is the migration entry point for `portzero trust
    /// generate`; the daemon deliberately does not call it (see
    /// [`load_or_create`], which only warns).
    pub fn ensure_name_constrained() -> Result<bool> {
        ensure_name_constrained_in(&data_dir()?)
    }

    /// Absolute path to the CA certificate PEM file.
    ///
    /// Pass this to the OS trust store installer so it can reference the file
    /// on disk rather than the in-memory PEM bytes.
    pub fn ca_cert_path() -> Result<PathBuf> {
        Ok(data_dir()?.join(CA_CERT))
    }
}

fn data_dir() -> Result<PathBuf> {
    Ok(base_data_dir()?.join("PortZero"))
}

fn base_data_dir() -> Result<PathBuf> {
    #[cfg(unix)]
    if let Some(home) = sudo_user_home() {
        return Ok(home.join(".local/share"));
    }

    dirs::data_dir().context("cannot resolve platform data directory")
}

#[cfg(unix)]
fn sudo_user_home() -> Option<PathBuf> {
    let sudo_user = std::env::var("SUDO_USER").ok()?;
    sudo_user_home_for(&sudo_user)
}

#[cfg(unix)]
fn sudo_user_home_for(sudo_user: &str) -> Option<PathBuf> {
    if sudo_user.is_empty() || sudo_user == "root" {
        return None;
    }
    passwd_home(sudo_user).or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
}

#[cfg(unix)]
fn passwd_home(username: &str) -> Option<PathBuf> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in content.lines() {
        let mut fields = line.splitn(7, ':');
        let name = fields.next()?;
        if name != username {
            continue;
        }
        let home = fields.nth(4)?;
        return Some(PathBuf::from(home));
    }
    None
}

/// Returns true when the persisted wildcard cert has more than
/// [`RENEW_WITHIN_DAYS`] of validity remaining.
fn is_fresh(dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(dir.join(WILDCARD_EXPIRY)) else {
        return false;
    };
    let Ok(expiry) = OffsetDateTime::parse(raw.trim(), &Rfc3339) else {
        return false;
    };
    expiry - OffsetDateTime::now_utc() > Duration::days(RENEW_WITHIN_DAYS)
}

fn has_browser_tls_usages(dir: &Path) -> bool {
    let Ok(ca_pem) = std::fs::read_to_string(dir.join(CA_CERT)) else {
        return false;
    };
    let Ok(leaf_pem) = std::fs::read_to_string(dir.join(WILDCARD_CERT)) else {
        return false;
    };
    certs_have_browser_tls_usages(&ca_pem, &leaf_pem)
}

fn certs_have_browser_tls_usages(ca_pem: &str, leaf_pem: &str) -> bool {
    let Ok(ca_params) = CertificateParams::from_ca_cert_pem(ca_pem) else {
        return false;
    };
    let Ok(leaf_params) = CertificateParams::from_ca_cert_pem(leaf_pem) else {
        return false;
    };

    ca_params.key_usages.contains(&KeyUsagePurpose::KeyCertSign)
        && ca_params.key_usages.contains(&KeyUsagePurpose::CrlSign)
        && ca_params
            .key_usages
            .contains(&KeyUsagePurpose::DigitalSignature)
        && leaf_params
            .key_usages
            .contains(&KeyUsagePurpose::DigitalSignature)
        && leaf_params
            .extended_key_usages
            .contains(&ExtendedKeyUsagePurpose::ServerAuth)
}

fn load(dir: &Path) -> Result<LocalCa> {
    Ok(LocalCa {
        ca_cert_pem: std::fs::read_to_string(dir.join(CA_CERT)).context("read ca.crt")?,
        wildcard_cert_pem: std::fs::read_to_string(dir.join(WILDCARD_CERT))
            .context("read wildcard.crt")?,
        wildcard_key_pem: std::fs::read_to_string(dir.join(WILDCARD_KEY))
            .context("read wildcard.key")?,
    })
}

fn save(ca: &LocalCa, expiry: OffsetDateTime, dir: &Path) -> Result<()> {
    // Each file is written to a sibling temp file and renamed into place, so a
    // regeneration replaces the old CA atomically: a concurrent reader (or a
    // crash mid-write) never sees a truncated or half-written cert. Key written
    // first with restricted permissions; if this fails the cert files are never
    // written so the bundle stays consistent on retry.
    crate::secure_file::write_secret_atomic(
        &dir.join(WILDCARD_KEY),
        ca.wildcard_key_pem.as_bytes(),
    )?;
    crate::secure_file::write_public_atomic(&dir.join(CA_CERT), ca.ca_cert_pem.as_bytes())
        .context("write ca.crt")?;
    crate::secure_file::write_public_atomic(
        &dir.join(WILDCARD_CERT),
        ca.wildcard_cert_pem.as_bytes(),
    )
    .context("write wildcard.crt")?;
    crate::secure_file::write_public_atomic(
        &dir.join(WILDCARD_EXPIRY),
        expiry
            .format(&Rfc3339)
            .context("format expiry timestamp")?
            .as_bytes(),
    )
    .context("write wildcard.expiry")?;
    Ok(())
}

fn generate() -> Result<(LocalCa, OffsetDateTime)> {
    let now = OffsetDateTime::now_utc();

    let mut ca_params = CertificateParams::new(vec![]).context("build CA cert params")?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.name_constraints = Some(ca_name_constraints());
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    ca_params.distinguished_name = make_dn("PortZero Local CA", Some("PortZero"));
    ca_params.not_before = now;
    ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
    let ca_key = KeyPair::generate().context("generate CA keypair")?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .context("self-sign CA certificate")?;

    let wildcard_expiry = now + Duration::days(WILDCARD_VALIDITY_DAYS);
    let mut leaf_params = CertificateParams::new(vec![
        "*.portzero.local".to_string(),
        "portzero.local".to_string(),
    ])
    .context("build wildcard cert params")?;
    leaf_params.distinguished_name = make_dn("*.portzero.local", None);
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.not_before = now;
    leaf_params.not_after = wildcard_expiry;
    let leaf_key = KeyPair::generate().context("generate wildcard keypair")?;
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .context("sign wildcard certificate")?;

    Ok((
        LocalCa {
            ca_cert_pem: ca_cert.pem(),
            wildcard_cert_pem: leaf_cert.pem(),
            wildcard_key_pem: leaf_key.serialize_pem(),
        },
        wildcard_expiry,
    ))
}

/// The X.509 Name Constraints the CA is issued with.
///
/// - `permitted_subtrees`: DNS names must fall within the `portzero.local`
///   subtree. A conforming verifier rejects any leaf whose DNS SAN lies outside
///   it, so a leaked CA key cannot mint a trusted cert for `example.com`.
/// - `excluded_subtrees`: all IPv4 and IPv6 addresses are excluded. A DNS
///   permitted subtree alone does not constrain a leaf that carries only an IP
///   SAN (RFC 5280 applies constraints per name-form), so we additionally
///   exclude the entire IP space. We never issue IP leaves, so this costs us
///   nothing and closes the IP-SAN bypass.
///
/// rcgen writes the Name Constraints extension as **critical** (see
/// `rcgen::certificate` — `oid::NAME_CONSTRAINTS` is emitted with the critical
/// flag set unconditionally), which is what RFC 5280 requires.
fn ca_name_constraints() -> NameConstraints {
    NameConstraints {
        permitted_subtrees: vec![GeneralSubtree::DnsName(CA_NAME_CONSTRAINT_DNS.to_string())],
        excluded_subtrees: vec![
            GeneralSubtree::IpAddress(CidrSubnet::from_v4_prefix([0, 0, 0, 0], 0)),
            GeneralSubtree::IpAddress(CidrSubnet::from_v6_prefix([0; 16], 0)),
        ],
    }
}

/// True when `ca_pem` carries a Name Constraints extension whose permitted DNS
/// subtree is exactly `portzero.local`. Used both to warn about legacy
/// (unconstrained) CAs and to decide whether to regenerate.
fn ca_pem_is_name_constrained(ca_pem: &str) -> bool {
    let Ok(params) = CertificateParams::from_ca_cert_pem(ca_pem) else {
        return false;
    };
    params.name_constraints.as_ref().is_some_and(|nc| {
        nc.permitted_subtrees.iter().any(|subtree| {
            matches!(subtree, GeneralSubtree::DnsName(dns) if dns == CA_NAME_CONSTRAINT_DNS)
        })
    })
}

/// True when the CA cert persisted in `dir` is name-constrained.
/// A missing or unreadable CA file counts as unconstrained.
fn dir_ca_is_name_constrained(dir: &Path) -> bool {
    let Ok(ca_pem) = std::fs::read_to_string(dir.join(CA_CERT)) else {
        return false;
    };
    ca_pem_is_name_constrained(&ca_pem)
}

/// Ensure the CA persisted in `dir` carries the name-constraints extension,
/// regenerating the whole bundle in place if a legacy (unconstrained) CA is
/// found. Returns `true` iff an existing CA was replaced because it lacked the
/// constraint — the caller should then tell the user to re-run `trust install`.
///
/// A fresh install (no CA on disk yet) generates a constrained CA and returns
/// `false`: that is the normal first-run path, not a migration.
fn ensure_name_constrained_in(dir: &Path) -> Result<bool> {
    crate::secure_file::ensure_private_dir(dir)
        .with_context(|| format!("create PortZero data dir: {}", dir.display()))?;

    let ca_exists = dir.join(CA_CERT).exists();
    if ca_exists && !dir_ca_is_name_constrained(dir) {
        tracing::info!("existing local CA has no name constraints; regenerating");
        let (ca, expiry) = generate()?;
        save(&ca, expiry, dir)?;
        return Ok(true);
    }

    // Already constrained, or absent — reuse when still fresh, else (re)create.
    if is_fresh(dir) && has_browser_tls_usages(dir) && dir_ca_is_name_constrained(dir) {
        return Ok(false);
    }
    let (ca, expiry) = generate()?;
    save(&ca, expiry, dir)?;
    Ok(false)
}

fn make_dn(common_name: &str, org: Option<&str>) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    if let Some(o) = org {
        dn.push(DnType::OrganizationName, o);
    }
    dn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_valid_pem() {
        let (ca, expiry) = generate().unwrap();
        assert!(ca.ca_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.wildcard_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.wildcard_key_pem.contains("BEGIN PRIVATE KEY"));
        assert!(expiry > OffsetDateTime::now_utc());
    }

    #[test]
    fn generate_adds_browser_tls_usages() {
        let Some(openssl) = openssl_available() else {
            eprintln!("skipping OpenSSL extension check: openssl not found");
            return;
        };

        let dir = tempfile::tempdir().unwrap();
        let (ca, _) = generate().unwrap();
        let ca_path = dir.path().join("ca.crt");
        let leaf_path = dir.path().join("wildcard.crt");
        std::fs::write(&ca_path, ca.ca_cert_pem).unwrap();
        std::fs::write(&leaf_path, ca.wildcard_cert_pem).unwrap();

        let ca_text = openssl_x509_text(&openssl, &ca_path);
        assert!(ca_text.contains("CA:TRUE"));
        assert!(ca_text.contains("Key Usage"));
        assert!(ca_text.contains("Certificate Sign"));
        assert!(ca_text.contains("CRL Sign"));
        assert!(ca_text.contains("Digital Signature"));
        // Name Constraints must be present, critical, and scope DNS to the
        // portzero.local subtree while excluding the whole IP space.
        assert!(
            ca_text.contains("X509v3 Name Constraints: critical"),
            "Name Constraints must be marked critical:\n{ca_text}"
        );
        assert!(ca_text.contains("Permitted:"));
        assert!(ca_text.contains("DNS:portzero.local"));
        assert!(ca_text.contains("Excluded:"));
        assert!(ca_text.contains("IP:0.0.0.0/0.0.0.0"));

        let leaf_text = openssl_x509_text(&openssl, &leaf_path);
        assert!(leaf_text.contains("DNS:*.portzero.local"));
        assert!(leaf_text.contains("DNS:portzero.local"));
        assert!(leaf_text.contains("Key Usage"));
        assert!(leaf_text.contains("Digital Signature"));
        assert!(leaf_text.contains("Extended Key Usage"));
        assert!(leaf_text.contains("TLS Web Server Authentication"));
    }

    #[test]
    fn generated_certs_satisfy_browser_tls_usage_check() {
        let (ca, _) = generate().unwrap();
        assert!(certs_have_browser_tls_usages(
            &ca.ca_cert_pem,
            &ca.wildcard_cert_pem
        ));
    }

    #[test]
    fn legacy_certs_without_usages_are_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let (ca, expiry) = generate_legacy_without_usages().unwrap();
        save(&ca, expiry, dir.path()).unwrap();

        assert!(is_fresh(dir.path()), "legacy cert is fresh by date");
        assert!(
            !has_browser_tls_usages(dir.path()),
            "legacy cert must be regenerated despite a fresh expiry file"
        );
    }

    #[test]
    fn round_trip_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let (ca, expiry) = generate().unwrap();
        save(&ca, expiry, dir.path()).unwrap();
        assert!(is_fresh(dir.path()), "freshly saved cert should be fresh");
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.ca_cert_pem, ca.ca_cert_pem);
        assert_eq!(loaded.wildcard_cert_pem, ca.wildcard_cert_pem);
        assert_eq!(loaded.wildcard_key_pem, ca.wildcard_key_pem);
    }

    #[test]
    fn stale_when_expiry_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_fresh(dir.path()));
    }

    #[test]
    fn stale_when_expiry_in_past() {
        let dir = tempfile::tempdir().unwrap();
        let past = OffsetDateTime::now_utc() - Duration::days(1);
        std::fs::write(
            dir.path().join(WILDCARD_EXPIRY),
            past.format(&Rfc3339).unwrap(),
        )
        .unwrap();
        assert!(!is_fresh(dir.path()));
    }

    #[test]
    fn stale_when_expiry_within_renew_window() {
        let dir = tempfile::tempdir().unwrap();
        let soon = OffsetDateTime::now_utc() + Duration::days(RENEW_WITHIN_DAYS - 1);
        std::fs::write(
            dir.path().join(WILDCARD_EXPIRY),
            soon.format(&Rfc3339).unwrap(),
        )
        .unwrap();
        assert!(!is_fresh(dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn sudo_user_home_ignores_root() {
        assert_eq!(sudo_user_home_for(""), None);
        assert_eq!(sudo_user_home_for("root"), None);
    }

    #[cfg(unix)]
    #[test]
    fn sudo_user_home_uses_passwd_home() {
        let current_user = std::env::var("USER").unwrap();
        let home = sudo_user_home_for(&current_user).unwrap();
        let expected_home = std::env::var("HOME").unwrap();
        assert_eq!(home, PathBuf::from(expected_home));
    }

    #[cfg(unix)]
    #[test]
    fn private_key_file_has_restricted_permissions() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.pem");
        crate::secure_file::write_secret_atomic(&path, b"test").unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "key file must be owner-read/write only"
        );
    }

    #[test]
    fn generated_ca_is_name_constrained() {
        let (ca, _) = generate().unwrap();
        assert!(
            ca_pem_is_name_constrained(&ca.ca_cert_pem),
            "freshly generated CA must carry the portzero.local name constraint"
        );
    }

    #[test]
    fn legacy_ca_is_not_name_constrained() {
        let (ca, _) = generate_legacy_without_usages().unwrap();
        assert!(
            !ca_pem_is_name_constrained(&ca.ca_cert_pem),
            "legacy CA has no name constraints and must be detected as such"
        );
    }

    #[test]
    fn ensure_name_constrained_replaces_legacy_ca() {
        let dir = tempfile::tempdir().unwrap();
        let (legacy, expiry) = generate_legacy_without_usages().unwrap();
        save(&legacy, expiry, dir.path()).unwrap();
        assert!(!dir_ca_is_name_constrained(dir.path()));

        let regenerated = ensure_name_constrained_in(dir.path()).unwrap();
        assert!(regenerated, "a legacy CA must be reported as regenerated");
        assert!(
            dir_ca_is_name_constrained(dir.path()),
            "the replacement CA must be name-constrained"
        );
        // Bundle stays internally consistent: the new leaf is signed by the new CA.
        assert!(has_browser_tls_usages(dir.path()));
    }

    #[test]
    fn ensure_name_constrained_keeps_constrained_ca() {
        let dir = tempfile::tempdir().unwrap();
        let (ca, expiry) = generate().unwrap();
        save(&ca, expiry, dir.path()).unwrap();

        let regenerated = ensure_name_constrained_in(dir.path()).unwrap();
        assert!(
            !regenerated,
            "an already-constrained CA must not be replaced"
        );
        let reloaded = load(dir.path()).unwrap();
        assert_eq!(
            reloaded.ca_cert_pem, ca.ca_cert_pem,
            "the constrained CA must be left byte-for-byte unchanged"
        );
    }

    #[test]
    fn ensure_name_constrained_creates_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let regenerated = ensure_name_constrained_in(dir.path()).unwrap();
        assert!(
            !regenerated,
            "a first-run create is not a migration and must return false"
        );
        assert!(dir_ca_is_name_constrained(dir.path()));
    }

    fn openssl_available() -> Option<String> {
        std::process::Command::new("openssl")
            .arg("version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| "openssl".to_string())
    }

    fn openssl_x509_text(openssl: &str, path: &Path) -> String {
        let output = std::process::Command::new(openssl)
            .args(["x509", "-in"])
            .arg(path)
            .args(["-noout", "-text"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "openssl x509 failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn generate_legacy_without_usages() -> Result<(LocalCa, OffsetDateTime)> {
        let now = OffsetDateTime::now_utc();

        let mut ca_params = CertificateParams::new(vec![]).context("build CA cert params")?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = make_dn("PortZero Local CA", Some("PortZero"));
        ca_params.not_before = now;
        ca_params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
        let ca_key = KeyPair::generate().context("generate CA keypair")?;
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .context("self-sign CA certificate")?;

        let wildcard_expiry = now + Duration::days(WILDCARD_VALIDITY_DAYS);
        let mut leaf_params = CertificateParams::new(vec![
            "*.portzero.local".to_string(),
            "portzero.local".to_string(),
        ])
        .context("build wildcard cert params")?;
        leaf_params.distinguished_name = make_dn("*.portzero.local", None);
        leaf_params.not_before = now;
        leaf_params.not_after = wildcard_expiry;
        let leaf_key = KeyPair::generate().context("generate wildcard keypair")?;
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .context("sign wildcard certificate")?;

        Ok((
            LocalCa {
                ca_cert_pem: ca_cert.pem(),
                wildcard_cert_pem: leaf_cert.pem(),
                wildcard_key_pem: leaf_key.serialize_pem(),
            },
            wildcard_expiry,
        ))
    }
}

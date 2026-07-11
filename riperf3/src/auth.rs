//! RSA authentication module — iperf3-compatible credential encryption and validation.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};

use crate::error::{Result, RiperfError};

/// Parse an RSA public key PEM the way OpenSSL's `PEM_read_PUBKEY` family
/// does: SPKI (`BEGIN PUBLIC KEY`) first, PKCS#1 (`BEGIN RSA PUBLIC KEY`) as
/// the fallback. GT accepts both via OpenSSL; parsing only SPKI silently
/// rejected PKCS#1 keys that work on iperf3 (#395).
pub(crate) fn parse_public_key_pem(pem: &[u8]) -> Result<RsaPublicKey> {
    let text = std::str::from_utf8(pem)
        .map_err(|e| RiperfError::Protocol(format!("invalid PEM encoding: {e}")))?;
    RsaPublicKey::from_public_key_pem(text)
        .or_else(|_| {
            use rsa::pkcs1::DecodeRsaPublicKey;
            RsaPublicKey::from_pkcs1_pem(text)
        })
        .map_err(|e| RiperfError::Protocol(format!("invalid RSA public key: {e}")))
}

/// Parse an RSA private key PEM like OpenSSL's `PEM_read_PrivateKey`:
/// PKCS#8 (`BEGIN PRIVATE KEY`) first, PKCS#1 (`BEGIN RSA PRIVATE KEY`) as
/// the fallback (#395, same tolerance rationale as [`parse_public_key_pem`]).
pub(crate) fn parse_private_key_pem(pem: &[u8]) -> Result<RsaPrivateKey> {
    let text = std::str::from_utf8(pem)
        .map_err(|e| RiperfError::Protocol(format!("invalid PEM encoding: {e}")))?;
    RsaPrivateKey::from_pkcs8_pem(text)
        .or_else(|_| {
            use rsa::pkcs1::DecodeRsaPrivateKey;
            RsaPrivateKey::from_pkcs1_pem(text)
        })
        .map_err(|e| RiperfError::Protocol(format!("invalid RSA private key: {e}")))
}

/// Validate that `path` reads and parses as an RSA PUBLIC key (`--username`
/// auth, client side). iperf3 loads the key at PARSE time and stamps
/// IESETCLIENTAUTH when it doesn't load (iperf_api.c:1854); the returned
/// error's text supplies the pre-error line's payload (#395).
pub fn validate_public_key_file(path: &std::path::Path) -> Result<()> {
    let pem = std::fs::read(path)
        .map_err(|e| RiperfError::Protocol(format!("cannot read RSA public key file: {e}")))?;
    parse_public_key_pem(&pem).map(|_| ())
}

/// Validate that `path` reads and parses as an RSA PRIVATE key (server-side
/// auth). iperf3 loads it at PARSE time and stamps IESETSERVERAUTH on
/// failure (iperf_api.c:1899) (#395).
pub fn validate_private_key_file(path: &std::path::Path) -> Result<()> {
    let pem = std::fs::read(path)
        .map_err(|e| RiperfError::Protocol(format!("cannot read RSA private key file: {e}")))?;
    parse_private_key_pem(&pem).map(|_| ())
}

/// Encode an auth token: encrypt `user: {u}\npwd:  {p}\nts:   {ts}` with the
/// server's RSA public key, then base64-encode the ciphertext.
pub fn encode_auth_token(
    username: &str,
    password: &str,
    pubkey_pem: &[u8],
    use_pkcs1: bool,
) -> Result<String> {
    let pubkey = parse_public_key_pem(pubkey_pem)?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Format must match iperf3 exactly: "user: %s\npwd:  %s\nts:   %lld"
    let plaintext = format!("user: {username}\npwd:  {password}\nts:   {ts}");

    let mut rng = rsa::rand_core::OsRng;
    let ciphertext = if use_pkcs1 {
        pubkey
            .encrypt(&mut rng, Pkcs1v15Encrypt, plaintext.as_bytes())
            .map_err(|e| RiperfError::Protocol(format!("RSA encrypt failed: {e}")))?
    } else {
        // OAEP with SHA-1 (matching iperf3's default RSA_PKCS1_OAEP_PADDING which uses SHA-1)
        use rsa::Oaep;
        pubkey
            .encrypt(&mut rng, Oaep::new::<sha1::Sha1>(), plaintext.as_bytes())
            .map_err(|e| RiperfError::Protocol(format!("RSA OAEP encrypt failed: {e}")))?
    };

    Ok(base64::engine::general_purpose::STANDARD.encode(&ciphertext))
}

/// Decode an auth token: base64-decode, RSA decrypt with private key,
/// parse username/password/timestamp from the plaintext.
pub fn decode_auth_token(
    token: &str,
    privkey_pem: &[u8],
    use_pkcs1: bool,
) -> Result<(String, String, i64)> {
    let privkey = parse_private_key_pem(privkey_pem)?;

    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(token)
        .map_err(|e| RiperfError::Protocol(format!("invalid base64 auth token: {e}")))?;

    let plaintext_bytes = if use_pkcs1 {
        privkey
            .decrypt(Pkcs1v15Encrypt, &ciphertext)
            .map_err(|e| RiperfError::Protocol(format!("RSA decrypt failed: {e}")))?
    } else {
        use rsa::Oaep;
        privkey
            .decrypt(Oaep::new::<sha1::Sha1>(), &ciphertext)
            .map_err(|e| RiperfError::Protocol(format!("RSA OAEP decrypt failed: {e}")))?
    };

    let plaintext = String::from_utf8(plaintext_bytes)
        .map_err(|e| RiperfError::Protocol(format!("auth plaintext not UTF-8: {e}")))?;

    // Parse: "user: {u}\npwd:  {p}\nts:   {ts}"
    let mut username = None;
    let mut password = None;
    let mut ts = None;

    for line in plaintext.lines() {
        if let Some(u) = line.strip_prefix("user: ") {
            username = Some(u.to_string());
        } else if let Some(p) = line.strip_prefix("pwd:  ") {
            password = Some(p.to_string());
        } else if let Some(t) = line.strip_prefix("ts:   ") {
            ts = t.trim().parse::<i64>().ok();
        }
    }

    match (username, password, ts) {
        (Some(u), Some(p), Some(t)) => Ok((u, p, t)),
        _ => Err(RiperfError::Protocol(
            "malformed auth plaintext: missing user/pwd/ts".into(),
        )),
    }
}

/// Validate credentials against an authorized-users file.
/// File format: `username,sha256hex` (one per line, # comments, empty lines skipped).
/// Password hash: `sha256("{username}{password}")`.
pub fn check_credentials(
    username: &str,
    password: &str,
    ts: i64,
    users_file: &str,
    skew_threshold: u32,
) -> Result<()> {
    // Check timestamp skew
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let skew = (now - ts).unsigned_abs();
    if skew > skew_threshold as u64 {
        return Err(RiperfError::Protocol(format!(
            "auth timestamp skew too large: {skew}s > {skew_threshold}s"
        )));
    }

    // Compute password hash: sha256("{username}{password}")
    let salted = format!("{{{username}}}{password}");
    let digest = Sha256::digest(salted.as_bytes());
    let hash = hex::encode(digest.as_slice());

    // Read and search the authorized users file
    let content = std::fs::read_to_string(users_file)
        .map_err(|e| RiperfError::Protocol(format!("cannot read authorized users file: {e}")))?;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((file_user, file_hash)) = line.split_once(',') {
            if file_user == username && file_hash == hash {
                return Ok(());
            }
        }
    }

    Err(RiperfError::AccessDenied)
}

/// Resolve password from multiple sources in priority order.
/// Returns the first non-None value, or None if all are absent.
pub fn resolve_password(
    riperf3_env: Option<&str>,
    iperf3_env: Option<&str>,
    interactive: Option<&str>,
) -> Option<String> {
    riperf3_env
        .or(iperf3_env)
        .or(interactive)
        .map(|s| s.to_string())
}

/// Read password from environment or interactive prompt.
/// Priority: RIPERF3_PASSWORD env → IPERF3_PASSWORD env → interactive prompt.
pub fn read_password() -> Result<String> {
    let riperf3_env = std::env::var("RIPERF3_PASSWORD").ok();
    let iperf3_env = std::env::var("IPERF3_PASSWORD").ok();
    if let Some(pw) = resolve_password(riperf3_env.as_deref(), iperf3_env.as_deref(), None) {
        return Ok(pw);
    }

    // GT's iperf_getpass (iperf_auth.c:442) runs tcgetattr BEFORE anything
    // else, so a NON-TTY stdin fails the whole read — no prompt, no line
    // consumed, even a piped password is refused (#395; live-probed: GT
    // headless with no env is IESETCLIENTAUTH, silently). Mirror that gate.
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() {
        return Err(RiperfError::Protocol(
            "password read failed: stdin is not a terminal".to_string(),
        ));
    }

    // Interactive prompt with echo disabled — safe, cross-platform. GT's
    // prompt goes to STDOUT (iperf_auth.c:455 printf) (#395).
    // #290 (r1 finding 2): a quiet run suppresses the PROMPT; the stdin
    // read itself is the pre-existing interactive wart.
    if !crate::macros::output_quiet() {
        use std::io::Write as _;
        print!("Password: ");
        let _ = std::io::stdout().flush();
    }
    rpassword::read_password()
        .map_err(|e| RiperfError::Protocol(format!("password read failed: {e}")))
}

/// Helper: format hex string (avoid pulling in the `hex` crate)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PUBKEY: &[u8] = include_bytes!("../tests/fixtures/test_public.pem");
    const TEST_PRIVKEY: &[u8] = include_bytes!("../tests/fixtures/test_private.pem");
    // #395 r1 F3: PKCS#1-FORMAT PEMs ("BEGIN RSA PRIVATE/PUBLIC KEY") — the
    // OpenSSL-tolerance fallback in the parse helpers. Distinct from the
    // PKCS#1-PADDING tests below (OAEP vs v1.5 on the same PKCS#8 keys).
    const TEST_PUBKEY_PKCS1: &[u8] = include_bytes!("../tests/fixtures/test_public_pkcs1.pem");
    const TEST_PRIVKEY_PKCS1: &[u8] = include_bytes!("../tests/fixtures/test_private_pkcs1.pem");

    #[test]
    fn pkcs1_format_pems_parse_via_the_fallback() {
        parse_public_key_pem(TEST_PUBKEY_PKCS1).expect("PKCS#1 public PEM parses");
        parse_private_key_pem(TEST_PRIVKEY_PKCS1).expect("PKCS#1 private PEM parses");
    }

    #[test]
    fn pkcs1_format_pems_round_trip_a_token() {
        let token = encode_auth_token("mario", "rossi", TEST_PUBKEY_PKCS1, false).unwrap();
        let (user, pass, _ts) = decode_auth_token(&token, TEST_PRIVKEY_PKCS1, false).unwrap();
        assert_eq!(user, "mario");
        assert_eq!(pass, "rossi");
    }

    #[test]
    fn pkcs1_format_pems_pass_file_validation() {
        let dir = std::env::temp_dir().join(format!("riperf3-auth-pkcs1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pub_p = dir.join("pub1.pem");
        let priv_p = dir.join("priv1.pem");
        std::fs::write(&pub_p, TEST_PUBKEY_PKCS1).unwrap();
        std::fs::write(&priv_p, TEST_PRIVKEY_PKCS1).unwrap();
        validate_public_key_file(&pub_p).expect("PKCS#1 public key file validates");
        validate_private_key_file(&priv_p).expect("PKCS#1 private key file validates");
    }

    #[test]
    fn encode_decode_round_trip_oaep() {
        let token = encode_auth_token("testuser", "testpass", TEST_PUBKEY, false).unwrap();
        assert!(!token.is_empty());

        let (user, pass, ts) = decode_auth_token(&token, TEST_PRIVKEY, false).unwrap();
        assert_eq!(user, "testuser");
        assert_eq!(pass, "testpass");

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((now - ts).unsigned_abs() < 5);
    }

    #[test]
    fn encode_decode_round_trip_pkcs1() {
        let token = encode_auth_token("alice", "secret123", TEST_PUBKEY, true).unwrap();
        let (user, pass, _ts) = decode_auth_token(&token, TEST_PRIVKEY, true).unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "secret123");
    }

    #[test]
    fn wrong_padding_fails() {
        let token = encode_auth_token("user", "pass", TEST_PUBKEY, false).unwrap();
        // Try to decrypt with PKCS1 when encrypted with OAEP
        let result = decode_auth_token(&token, TEST_PRIVKEY, true);
        assert!(result.is_err());
    }

    #[test]
    fn check_credentials_valid() {
        let users_file = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test_users.csv");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let result = check_credentials("testuser", "testpass", now, users_file, 10);
        assert!(result.is_ok(), "valid credentials should pass: {result:?}");
    }

    #[test]
    fn check_credentials_wrong_password() {
        let users_file = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test_users.csv");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let result = check_credentials("testuser", "wrongpass", now, users_file, 10);
        assert!(result.is_err(), "wrong password should fail");
    }

    #[test]
    fn check_credentials_unknown_user() {
        let users_file = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test_users.csv");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let result = check_credentials("nobody", "testpass", now, users_file, 10);
        assert!(result.is_err(), "unknown user should fail");
    }

    #[test]
    fn check_credentials_timestamp_skew() {
        let users_file = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test_users.csv");
        let old_ts = 1000000000; // year 2001
        let result = check_credentials("testuser", "testpass", old_ts, users_file, 10);
        assert!(result.is_err(), "stale timestamp should fail");
    }

    #[test]
    fn hex_encode_correctness() {
        assert_eq!(hex::encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
        assert_eq!(hex::encode(&[0x00, 0xFF]), "00ff");
    }

    #[test]
    fn password_hash_format() {
        // Verify our hash matches what iperf3 produces
        // iperf3: sha256("{testuser}testpass")
        let salted = "{testuser}testpass";
        let hash = hex::encode(Sha256::digest(salted.as_bytes()).as_slice());
        assert_eq!(
            hash,
            "6d30222cf5cb9f09b0175e1dbfbc0b6fef34fc08c2fdf02682e0c2450c9c7170"
        );
    }

    // -- Password resolution priority --

    #[test]
    fn resolve_password_riperf3_env_wins() {
        let result = resolve_password(
            Some("from_riperf3"),
            Some("from_iperf3"),
            Some("from_prompt"),
        );
        assert_eq!(result, Some("from_riperf3".to_string()));
    }

    #[test]
    fn resolve_password_iperf3_env_fallback() {
        let result = resolve_password(None, Some("from_iperf3"), Some("from_prompt"));
        assert_eq!(result, Some("from_iperf3".to_string()));
    }

    #[test]
    fn resolve_password_prompt_fallback() {
        let result = resolve_password(None, None, Some("from_prompt"));
        assert_eq!(result, Some("from_prompt".to_string()));
    }

    #[test]
    fn resolve_password_none_when_all_absent() {
        let result = resolve_password(None, None, None);
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_password_riperf3_takes_priority_over_iperf3() {
        // Even if both are set, RIPERF3 wins
        let result = resolve_password(Some("riperf3_pw"), Some("iperf3_pw"), None);
        assert_eq!(result, Some("riperf3_pw".to_string()));
    }
}

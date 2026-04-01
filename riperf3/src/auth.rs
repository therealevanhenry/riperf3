//! RSA authentication module — iperf3-compatible credential encryption and validation.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};

use crate::error::{RiperfError, Result};

/// Encode an auth token: encrypt `user: {u}\npwd:  {p}\nts:   {ts}` with the
/// server's RSA public key, then base64-encode the ciphertext.
pub fn encode_auth_token(
    username: &str,
    password: &str,
    pubkey_pem: &[u8],
    use_pkcs1: bool,
) -> Result<String> {
    let pubkey = RsaPublicKey::from_public_key_pem(
        std::str::from_utf8(pubkey_pem)
            .map_err(|e| RiperfError::Protocol(format!("invalid PEM encoding: {e}")))?,
    )
    .map_err(|e| RiperfError::Protocol(format!("invalid RSA public key: {e}")))?;

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
    let privkey = RsaPrivateKey::from_pkcs8_pem(
        std::str::from_utf8(privkey_pem)
            .map_err(|e| RiperfError::Protocol(format!("invalid PEM encoding: {e}")))?,
    )
    .map_err(|e| RiperfError::Protocol(format!("invalid RSA private key: {e}")))?;

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
    let content = std::fs::read_to_string(users_file).map_err(|e| {
        RiperfError::Protocol(format!("cannot read authorized users file: {e}"))
    })?;

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

/// Read password from environment or interactive prompt.
/// Checks RIPERF3_PASSWORD first, then IPERF3_PASSWORD, then prompts.
pub fn read_password() -> Result<String> {
    if let Ok(pw) = std::env::var("RIPERF3_PASSWORD") {
        return Ok(pw);
    }
    if let Ok(pw) = std::env::var("IPERF3_PASSWORD") {
        return Ok(pw);
    }

    // Interactive prompt with echo disabled
    eprint!("Password: ");
    #[cfg(unix)]
    {
        use std::io::{BufRead, Write};
        std::io::stderr().flush().ok();

        // Disable echo
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        let stdin_fd = libc::STDIN_FILENO;
        unsafe {
            libc::tcgetattr(stdin_fd, termios.as_mut_ptr());
        }
        let mut termios = unsafe { termios.assume_init() };
        let old_lflag = termios.c_lflag;
        termios.c_lflag &= !libc::ECHO;
        unsafe {
            libc::tcsetattr(stdin_fd, libc::TCSANOW, &termios);
        }

        let mut password = String::new();
        std::io::stdin().lock().read_line(&mut password).ok();

        // Restore echo
        termios.c_lflag = old_lflag;
        unsafe {
            libc::tcsetattr(stdin_fd, libc::TCSANOW, &termios);
        }
        eprintln!(); // newline after password

        Ok(password.trim_end().to_string())
    }

    #[cfg(not(unix))]
    {
        use std::io::BufRead;
        let mut password = String::new();
        std::io::stdin().lock().read_line(&mut password).ok();
        Ok(password.trim_end().to_string())
    }
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

    #[test]
    fn encode_decode_round_trip_oaep() {
        let token =
            encode_auth_token("testuser", "testpass", TEST_PUBKEY, false).unwrap();
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
        let token =
            encode_auth_token("alice", "secret123", TEST_PUBKEY, true).unwrap();
        let (user, pass, _ts) = decode_auth_token(&token, TEST_PRIVKEY, true).unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "secret123");
    }

    #[test]
    fn wrong_padding_fails() {
        let token =
            encode_auth_token("user", "pass", TEST_PUBKEY, false).unwrap();
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
        assert_eq!(hash, "6d30222cf5cb9f09b0175e1dbfbc0b6fef34fc08c2fdf02682e0c2450c9c7170");
    }
}

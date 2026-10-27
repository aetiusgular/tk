//! Tokens: the daemon's own token (in its socket's `hello`) and one per chat
//! (in `x-tk-token` on the OTLP receiver, in `TK_CHAT_TOKEN` for `tk run`).
//! A token is 32 random bytes as 64 hex characters; the daemon stores only
//! `hash(token)`. Never in a command-line argument (rules, section 5, item
//! 1; the fork's `harness_env.rs:1-15`).

use sha2::{Digest, Sha256};
use std::io::Read;

pub const TOKEN_LEN: usize = 64;

/// Mints a token from the OS random source. Returns an error instead of a
/// weak token when the source cannot be read.
pub fn mint() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

/// SHA-256 of the token, as hex: what the daemon keeps and compares.
pub fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// True when `token` is well formed: exactly 64 lowercase hex characters.
pub fn well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_tokens_are_well_formed_and_distinct() {
        let a = mint().unwrap();
        let b = mint().unwrap();
        assert!(well_formed(&a));
        assert!(well_formed(&b));
        assert_ne!(a, b);
    }

    #[test]
    fn hash_is_stable_and_not_the_token() {
        let t = "a".repeat(64);
        assert_eq!(hash(&t), hash(&t));
        assert_ne!(hash(&t), t);
        assert_eq!(hash(&t).len(), 64);
    }

    #[test]
    fn malformed_tokens_are_refused() {
        assert!(!well_formed("ABC"));
        assert!(!well_formed(&"A".repeat(64)));
        assert!(!well_formed(&"a".repeat(63)));
    }
}

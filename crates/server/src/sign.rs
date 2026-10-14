//! The server's signature of its own translations, so that /sync can tell them from the ones an
//! addon makes up (trust 1 against 0, schema.sql).
//!
//! /translate signs each text it translates: HMAC-SHA256, with a secret that never leaves the
//! server, of the XML sent and the XML answered. The addon keeps both with the translation and
//! sends them back with it; /sync checks the signature, then makes the text the same way the addon
//! does (gw2it-text) from the English and the answered XML: only if it comes out equal to the text
//! received is it the server's own. An addon, or anybody, cannot sign a text the server did not
//! translate, and cannot change one it did.

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

/// The secret, kept in the database (table `meta`): made at the first run.
pub fn secret(con: &Connection) -> rusqlite::Result<Vec<u8>> {
    con.execute_batch("CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);")?;
    let found: Option<String> = con.query_row("SELECT v FROM meta WHERE k = 'sign_secret'", [], |r| r.get(0)).optional()?;
    if let Some(hex) = found {
        return Ok((0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect());
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    con.execute("INSERT INTO meta (k, v) VALUES ('sign_secret', ?1)", [hex])?;
    Ok(bytes.to_vec())
}

fn hmac(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// The signature of a translation: `sent` is the XML the translator got, `answered` what it gave.
pub fn sign(secret: &[u8], sent: &str, answered: &str) -> String {
    hmac(secret, &[sent.as_bytes(), &[0], answered.as_bytes()]).iter().map(|b| format!("{b:02x}")).collect()
}

/// True if `it`, the text an addon sends for `en`, is what the server's `answered` XML makes of it,
/// and `signature` proves that the server gave that answer.
pub fn verify(secret: &[u8], en: &str, it: &str, signature: &str, answered: &str) -> bool {
    let Some(prepared) = gw2it_text::markup::prepare(en) else {
        return false;
    };
    let expected = sign(secret, &prepared.xml, answered);
    // Not the first difference decides how long it takes.
    let same = expected.len() == signature.len() && expected.bytes().zip(signature.bytes()).fold(0, |d, (a, b)| d | (a ^ b)) == 0;
    same && gw2it_text::finish(en, &prepared, answered).as_deref() == Some(it)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        // key "Jefe", data "what do ya want for nothing?"
        let mac = hmac(b"Jefe", &[b"what do ya want for nothing?"]);
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn only_what_the_server_answered_verifies() {
        let secret = b"secret";
        let en = "Sword of %str1%";
        let xml = gw2it_text::markup::prepare(en).unwrap().xml;
        let answered = "Spada di <x id=\"0\"/>";
        let signature = sign(secret, &xml, answered);
        let it = "Spada di %str1%";
        assert!(verify(secret, en, it, &signature, answered));
        // Another text, another answer, another secret, a signature for another English.
        assert!(!verify(secret, en, "Spada falsa %str1%", &signature, answered));
        assert!(!verify(secret, en, it, &signature, "Lama di <x id=\"0\"/>"));
        assert!(!verify(b"other", en, it, &signature, answered));
        assert!(!verify(secret, "Axe of %str1%", "Ascia di %str1%", &signature, answered));
    }
}

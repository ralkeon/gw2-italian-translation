//! What turns a translator's answer into the text the game shows: the same code in the addon, which
//! does it, and in the server, which does it again to check that a translation it is sent really
//! is one of its own (see `sign` in crates/server).

pub mod consistency;
pub mod glossary;
pub mod markup;
pub mod names;
pub mod plural;

/// The text the game shows for `en`, from the translator's XML answer `xml` (None if it lost,
/// repeated or invented a placeholder). `prepared` comes from `markup::prepare(en)`.
pub fn finish(en: &str, prepared: &markup::Prepared, xml: &str) -> Option<String> {
    let it = prepared.restore(xml)?;
    Some(plural::pluralize(en, &markup::fix_spacing(en, &it)))
}

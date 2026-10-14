//! GW2 markup in source texts, and its round trip through a translator that handles XML.
//!
//! Tokens the translation must keep verbatim (`%str1%`, `%%`, `<c=#A594F7>`, `</c>`,
//! `[lbracket]`, `[null]`, `[f]`, line breaks...) become `<x id="N"/>` placeholders and are put
//! back afterwards. English grammar markers are dropped before translating: `[s]` (plural ending),
//! `[pl:"..."]` (irregular plural), `[f:"..."]`/`[m:"..."]` (gendered form), so the translation
//! is always the singular, masculine form.

pub struct Prepared {
    /// Text to translate, XML-escaped, with placeholders.
    pub xml: String,
    tokens: Vec<String>,
}

/// None when there is nothing to translate (only placeholders, numbers, punctuation).
pub fn prepare(en: &str) -> Option<Prepared> {
    let mut xml = String::with_capacity(en.len() + 16);
    let mut tokens = Vec::new();
    let mut has_words = false;
    let mut rest = en;
    while let Some(c) = rest.chars().next() {
        if let Some(len) = token_len(rest) {
            let token = &rest[..len];
            if !is_grammar(token) {
                xml.push_str(&format!("<x id=\"{}\"/>", tokens.len()));
                tokens.push(token.to_owned());
            }
            rest = &rest[len..];
            continue;
        }
        has_words |= c.is_alphabetic();
        match c {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            _ => xml.push(c),
        }
        rest = &rest[c.len_utf8()..];
    }
    has_words.then_some(Prepared { xml, tokens })
}

impl Prepared {
    /// The translation with the tokens back in place. None if the translator lost, repeated or
    /// invented a placeholder.
    pub fn restore(&self, xml: &str) -> Option<String> {
        let mut out = String::with_capacity(xml.len() + 16);
        let mut used = vec![false; self.tokens.len()];
        let mut rest = xml;
        while let Some(c) = rest.chars().next() {
            if c == '<' {
                let end = rest.find('>')?;
                let id = parse_placeholder(&rest[..=end])?;
                if *used.get(id)? {
                    return None;
                }
                used[id] = true;
                out.push_str(&self.tokens[id]);
                rest = &rest[end + 1..];
            } else if c == '&' {
                let end = rest.find(';')?;
                out.push(match &rest[..=end] {
                    "&amp;" => '&',
                    "&lt;" => '<',
                    "&gt;" => '>',
                    "&quot;" => '"',
                    "&apos;" => '\'',
                    _ => return None,
                });
                rest = &rest[end + 1..];
            } else {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        used.iter().all(|&u| u).then_some(out)
    }
}

/// The fixed tokens of `en` missing from `it` or added to it (grammar markers are free), for a
/// hand-written translation. Empty when the markup is intact.
pub fn token_mismatch(en: &str, it: &str) -> Vec<String> {
    let (mut want, have) = (fixed_tokens(en), fixed_tokens(it));
    let mut extra = Vec::new();
    for t in have {
        match want.iter().position(|w| *w == t) {
            Some(i) => {
                want.swap_remove(i);
            }
            None => extra.push(format!("+{t}")),
        }
    }
    want.into_iter().map(|t| format!("-{t}")).chain(extra).collect()
}

/// Puts back the spaces a translator dropped next to the tokens of `en`: `Level %num1% %str1%`
/// came back as `Livello  %num1%%str1%`, and the game no longer saw the `[f]` that `%str1%`
/// carries as a gender mark. Doubled spaces are collapsed.
pub fn fix_spacing(en: &str, it: &str) -> String {
    let mut out = it.to_owned();
    let mut from = 0;
    let mut rest = en;
    let mut offset = 0;
    while let Some(c) = rest.chars().next() {
        let Some(len) = token_len(rest) else {
            rest = &rest[c.len_utf8()..];
            offset += c.len_utf8();
            continue;
        };
        let token = &rest[..len];
        let (before, after) = (en[..offset].ends_with(' '), en[offset + len..].starts_with(' '));
        rest = &rest[len..];
        offset += len;
        if is_grammar(token) || token == "
" || token.starts_with('<') {
            continue;
        }
        let Some(at) = out[from..].find(token).map(|i| i + from) else { continue };
        let mut end = at + len;
        if after && out[end..].chars().next().is_some_and(|c| !c.is_whitespace() && !".,;:!?)»".contains(c)) {
            out.insert(end, ' ');
        }
        if before && out[..at].chars().next_back().is_some_and(|c| !c.is_whitespace() && !"(«".contains(c)) {
            out.insert(at, ' ');
            end += 1;
        }
        from = end;
    }
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out
}

fn fixed_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        match token_len(rest) {
            Some(len) => {
                if !is_grammar(&rest[..len]) {
                    tokens.push(&rest[..len]);
                }
                rest = &rest[len..];
            }
            None => rest = &rest[c.len_utf8()..],
        }
    }
    tokens
}

/// Length of the markup token at the start of `s`, if there is one.
fn token_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    match b[0] {
        b'\n' => Some(1),
        b'%' => {
            let n = b[1..].iter().take_while(|c| c.is_ascii_alphanumeric()).count();
            (b.get(1 + n) == Some(&b'%')).then_some(n + 2)
        }
        b'<' => s.find('>').map(|i| i + 1),
        b'[' => s.find(']').map(|i| i + 1),
        _ => None,
    }
}

fn is_grammar(token: &str) -> bool {
    let Some(inner) = token.strip_prefix('[').and_then(|t| t.strip_suffix(']')) else {
        return false;
    };
    inner == "s" || ["pl:", "f:", "m:"].iter().any(|p| inner.starts_with(p))
}

/// `<x id="3"/>` (or `<x id="3"></x>` collapsed by the caller: not accepted) -> 3.
fn parse_placeholder(tag: &str) -> Option<usize> {
    tag.strip_prefix("<x id=\"")?.strip_suffix("\"/>")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(en: &str, fake_translation: impl Fn(&str) -> String) -> Option<String> {
        let p = prepare(en)?;
        p.restore(&fake_translation(&p.xml))
    }

    #[test]
    fn placeholders_survive() {
        let p = prepare("<c=#A594F7>Legendary & Co</c>").unwrap();
        assert_eq!(p.xml, "<x id=\"0\"/>Legendary &amp; Co<x id=\"1\"/>");
        assert_eq!(p.restore("<x id=\"0\"/>Leggendario &amp; Co<x id=\"1\"/>").unwrap(), "<c=#A594F7>Leggendario & Co</c>");
    }

    #[test]
    fn percent_tokens() {
        let p = prepare("%num1%%% WvW Bonus, 50% off").unwrap();
        assert_eq!(p.xml, "<x id=\"0\"/><x id=\"1\"/> WvW Bonus, 50% off");
        assert_eq!(round_trip("Delivered %num1%/%num2% Books", |x| x.replace("Delivered", "Consegnati")).unwrap(), "Consegnati %num1%/%num2% Books");
    }

    #[test]
    fn grammar_markers_dropped() {
        assert_eq!(prepare("Juvenile Smokescale[s]").unwrap().xml, "Juvenile Smokescale");
        assert_eq!(prepare("Mini[pl:\"Minis of\"] Qadim").unwrap().xml, "Mini Qadim");
        assert_eq!(prepare("Raid God[f:\"Goddess\"]").unwrap().xml, "Raid God");
        assert_eq!(prepare("[f]%str1%").map(|p| p.xml), None);
    }

    #[test]
    fn nothing_to_translate() {
        for en in ["[null]", " [null]", "%str1%: %str2%", "((912041))", "\n[b]", "<c=%str1%>%str2%</c>"] {
            assert!(prepare(en).is_none(), "{en}");
        }
    }

    #[test]
    fn hand_written_markup() {
        let en = "<c=@flavor>%str1% Fox[s]</c>\n[b]";
        assert!(token_mismatch(en, "<c=@flavor>Volp[pl:\"Volpi\"]e %str1%</c>\n[b]").is_empty());
        assert_eq!(token_mismatch(en, "<c=@flavor>Volpe</c> %str2%\n[b]"), ["-%str1%", "+%str2%"]);
    }

    #[test]
    fn spacing_restored() {
        assert_eq!(fix_spacing("Level %num1% %str1%", "Livello  %num1%%str1%"), "Livello %num1% %str1%");
        assert_eq!(fix_spacing("Level %num1% Rewards", "Livello  %num1%Ricompense"), "Livello %num1% Ricompense");
        assert_eq!(fix_spacing("Done %num1%/%num2%", "Fatto %num1%/%num2%"), "Fatto %num1%/%num2%");
        assert_eq!(fix_spacing("Cost: %str1% .", "Costo: %str1%."), "Costo: %str1%.");
        assert_eq!(fix_spacing("Bonus", "Bonus"), "Bonus");
    }

    #[test]
    fn broken_placeholders_rejected() {
        let p = prepare("%str1% is now %str2%.").unwrap();
        assert!(p.restore("<x id=\"0\"/> è ora.").is_none()); // lost one
        assert!(p.restore("<x id=\"0\"/> <x id=\"0\"/> <x id=\"1\"/>").is_none()); // repeated
        assert!(p.restore("<x id=\"0\"/> è <b>ora</b> <x id=\"1\"/>").is_none()); // invented
        assert_eq!(p.restore("<x id=\"0\"/> è ora <x id=\"1\"/>.").unwrap(), "%str1% è ora %str2%.");
    }
}

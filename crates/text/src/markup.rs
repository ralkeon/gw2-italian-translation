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

/// `en` as `opens + body + closes`, when it is a name or a sentence wrapped in tokens
/// (`%str1%%str2%Seer Pants%str3%%str4%`, `<c=#ccbb66>Eternal Forgemaster</c>`): the body has no
/// token of its own (grammar marks apart) and both ends have at least one.
fn wrapped(en: &str) -> Option<(&str, &str)> {
    // (start, length) of each fixed token, in order, and whether anything else is between them.
    let mut tokens = Vec::new();
    let mut rest = en;
    let mut at = 0;
    while let Some(c) = rest.chars().next() {
        let len = token_len(rest).unwrap_or(c.len_utf8());
        if token_len(rest).is_some() && !is_grammar(&rest[..len]) && &rest[..len] != "
" {
            tokens.push((at, len));
        }
        rest = &rest[len..];
        at += len;
    }
    // The leading run of tokens ends where the first gap starts; the trailing one begins at the last gap.
    let mut open_end = 0;
    while let Some(&(s, l)) = tokens.iter().find(|(s, _)| *s == open_end) {
        open_end = s + l;
    }
    let mut close_start = en.len();
    while let Some(&(s, _)) = tokens.iter().find(|(s, l)| s + l == close_start) {
        close_start = s;
    }
    let body = en.get(open_end..close_start)?;
    let body_has_token = tokens.iter().any(|(s, _)| *s >= open_end && *s < close_start);
    (open_end > 0 && close_start < en.len() && !body_has_token && body.chars().any(char::is_alphabetic))
        .then(|| (&en[..open_end], &en[close_start..]))
}

/// Whether the wrapper of `en` (see `wrapped`) is broken in `it`: the opening tokens not at the
/// start, the closing ones not at the end, or a `</c>` before the `<c=...>` it closes.
pub fn wrap_broken(en: &str, it: &str) -> bool {
    if let Some((open, close)) = wrapped(en) {
        let it = it.trim();
        if !it.starts_with(open) || !it.ends_with(close) {
            return true;
        }
    }
    // Colour spans: judged only where the English itself is balanced.
    c_balanced(en) && !c_balanced(it)
}

/// Every `</c>` comes after the `<c=...>` it closes, and none is left open.
fn c_balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = token_len(rest).unwrap_or(c.len_utf8());
        match &rest[..len] {
            t if t.starts_with("<c=") => depth += 1,
            "</c>" => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
        rest = &rest[len..];
    }
    depth == 0
}

/// `it` with the wrapper of `en` put back around the whole text: a translator moved the closing
/// tokens inside the sentence (`Gilet%str3%%str4% %str1%%str2%di pelle`). `it` as it is if `en` is
/// not wrapped, the wrapper is fine, or the tokens are not the ones of `en`.
pub fn fix_wrap(en: &str, it: &str) -> String {
    let Some((open, close)) = wrapped(en) else { return it.to_owned() };
    if !wrap_broken(en, it) || !token_mismatch(en, it).is_empty() {
        return it.to_owned();
    }
    let mut bare = String::with_capacity(it.len());
    let mut rest = it;
    while let Some(c) = rest.chars().next() {
        let len = token_len(rest).unwrap_or(c.len_utf8());
        let token = &rest[..len];
        if !(token_len(rest).is_some() && (open.contains(token) || close.contains(token)) && !is_grammar(token)) {
            bare.push_str(token);
        }
        rest = &rest[len..];
    }
    while bare.contains("  ") {
        bare = bare.replace("  ", " ");
    }
    let out = format!("{open}{}{close}", bare.trim());
    if token_mismatch(en, &out).is_empty() && !wrap_broken(en, &out) { out } else { it.to_owned() }
}

/// Everything wrong with the markup that can be put right from the English: the wrapper
/// (`fix_wrap`) and the `%` signs (`fix_percent`).
pub fn repair(en: &str, it: &str) -> String {
    fix_percent(en, &fix_wrap(en, it))
}

/// A translator's answer after `repair` and the spaces put back; None if the markup is still
/// broken (the game would show it wrong: better English than that).
pub fn finish(en: &str, it: &str) -> Option<String> {
    let it = repair(en, &fix_spacing(en, &fix_wrap(en, it)));
    (!wrap_broken(en, &it) && !stray_percent(&it)).then_some(it)
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

/// Whether `it` has a `%` outside the tokens (`%%`, `%str1%`). The game reads it as the start of
/// a format (`% d` is `%d`) and cuts the text there: "66%% % di probabilità" showed "66%".
pub fn stray_percent(it: &str) -> bool {
    let mut rest = it;
    while let Some(c) = rest.chars().next() {
        match token_len(rest) {
            Some(len) => rest = &rest[len..],
            None if c == '%' => return true,
            None => rest = &rest[c.len_utf8()..],
        }
    }
    false
}

/// `it` with the percent signs a translator added or moved put back after the numbers they follow
/// in `en`: "25%% % di" and "del%% 75%" (DeepL sees a percentage and adds its own sign). `it` as
/// it is if it has no stray `%`, or if the numbers cannot be found.
pub fn fix_percent(en: &str, it: &str) -> String {
    if !stray_percent(it) {
        return it.to_owned();
    }
    // The numbers `%%` follows in the English, in order; a `%%` after anything else: give up.
    let mut numbers = Vec::new();
    let mut rest = en;
    let mut at = 0;
    while let Some(c) = rest.chars().next() {
        let len = token_len(rest).unwrap_or(c.len_utf8());
        if &rest[..len] == "%%" {
            let number: String = en[..at].chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',').collect();
            let number: String = number.chars().rev().collect::<String>().trim_start_matches(['.', ',']).to_owned();
            if number.is_empty() {
                return it.to_owned();
            }
            numbers.push(number);
        }
        rest = &rest[len..];
        at += len;
    }
    // Without any percent sign, then the signs back after the numbers.
    let mut bare = String::with_capacity(it.len());
    let mut rest = it;
    while let Some(c) = rest.chars().next() {
        match token_len(rest) {
            Some(2) if rest.starts_with("%%") => rest = &rest[2..],
            Some(len) => {
                bare.push_str(&rest[..len]);
                rest = &rest[len..];
            }
            None if c == '%' => rest = &rest[1..],
            None => {
                bare.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    while bare.contains("  ") {
        bare = bare.replace("  ", " ");
    }
    let mut out = String::with_capacity(bare.len() + 2 * numbers.len());
    let mut from = 0;
    for number in &numbers {
        let found = bare[from..].match_indices(number.as_str()).map(|(i, _)| from + i).find(|&i| {
            let end = i + number.len();
            !bare[..i].ends_with(|c: char| c.is_ascii_digit()) && !bare[end..].starts_with(|c: char| c.is_ascii_digit())
        });
        let Some(i) = found else { return it.to_owned() };
        let end = i + number.len();
        out.push_str(&bare[from..end]);
        out.push_str("%%");
        from = end;
    }
    out.push_str(&bare[from..]);
    if token_mismatch(en, &out).is_empty() && !stray_percent(&out) { out } else { it.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stray_percent_signs() {
        assert!(stray_percent("66%% % di probabilità"));
        assert!(!stray_percent("66%% di probabilità, %str1%"));
        assert_eq!(fix_percent("66%% Chance to Steal Life", "66%% % di probabilità di rubare vita"), "66%% di probabilità di rubare vita");
        assert_eq!(
            fix_percent("Gain a 75%% chance to steal life", "Ottieni una probabilità del%% 75% di rubare vita"),
            "Ottieni una probabilità del 75%% di rubare vita"
        );
        assert_eq!(
            fix_percent("Reduced by 50%%. 50%% of hits", "Ridotta di 50%%. Il 50%% % dei colpi"),
            "Ridotta di 50%%. Il 50%% dei colpi"
        );
        // Nothing stray, or a number that is not there: as it is.
        assert_eq!(fix_percent("10%% more", "10%% in più"), "10%% in più");
        assert_eq!(fix_percent("10%% more", "dieci % in più"), "dieci % in più");
    }

    #[test]
    fn wrapper_kept_around_the_text() {
        let en = "%str1%%str2%Seer Pants%str3%%str4%";
        let bad = "Pantaloni%str3%%str4% %str1%%str2%del Veggente";
        assert!(wrap_broken(en, bad));
        assert_eq!(fix_wrap(en, bad), "%str1%%str2%Pantaloni del Veggente%str3%%str4%");
        assert!(!wrap_broken(en, "%str1%%str2%Pantaloni del Veggente%str3%%str4%"));
        assert_eq!(fix_wrap("<c=#ccbb66>Eternal Forgemaster</c>", "Maestro</c> della Fucina <c=#ccbb66>Eterna"), "<c=#ccbb66>Maestro della Fucina Eterna</c>");
        // Grammar marks stay where they are.
        assert_eq!(
            fix_wrap("%str1%%str2%Ruby Shard[s]%str3%%str4%", "Frammento[pl:\"Frammenti\"]%str3%%str4% %str1%%str2%di rubino"),
            "%str1%%str2%Frammento[pl:\"Frammenti\"] di rubino%str3%%str4%"
        );
        // Not wrapped, or a span inside a sentence: left alone (judged by wrap_broken).
        assert_eq!(fix_wrap("%str1% is now %str2%.", "%str2% è ora %str1%."), "%str2% è ora %str1%.");
        assert!(!wrap_broken("%str1% is now %str2%.", "%str2% è ora %str1%."));
        let en = "<c=@abilitytype>Dual Attack</c> skills weaken enemies.";
        assert!(wrap_broken(en, "Le abilità di \"Attacco</c> <c=@abilitytype>doppio\" indeboliscono."));
        assert!(!wrap_broken(en, "Le abilità <c=@abilitytype>Attacco doppio</c> indeboliscono."));
        // English already starts inside a span: its closing first is not a mistake.
        assert!(!wrap_broken("Hits.</c><br><c=@reminder>Once.</c>", "Colpi.</c><br><c=@reminder>Una volta.</c>"));
        assert_eq!(fix_wrap(en, "Le abilità di \"Attacco</c> <c=@abilitytype>doppio\" indeboliscono."), "Le abilità di \"Attacco</c> <c=@abilitytype>doppio\" indeboliscono.");
    }

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

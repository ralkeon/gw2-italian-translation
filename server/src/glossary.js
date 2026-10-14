// The glossary's rules, the same as crates/text/src/glossary.rs (merge, Term::is_valid) and
// crates/server/src/glossary.rs (term_of): see there. Plain JavaScript with no imports, so that
// test.mjs runs it too.

const FORM_MAX = 40;
const WRONG_MAX = 20;
const SPAN_MAX = 3;
const SHARED_MIN = 4;

const same = (a, b) => a.toLowerCase() === b.toLowerCase();
const PLAIN = /^[\p{L}'’-]+( [\p{L}'’-]+)*$/u;

/** Letters, single spaces, apostrophes and hyphens, a few words. */
function plain(form) {
  return (
    typeof form === "string" &&
    [...form].length <= FORM_MAX &&
    form.split(" ").length <= SPAN_MAX &&
    PLAIN.test(form)
  );
}

/** A term sent by an addon: [en, it, [wrong...]] -> {en, it, wrong}, or null if it is not valid. */
export function termOf(row) {
  if (!Array.isArray(row) || row.length !== 3) return null;
  const [en, it, wrong] = row;
  if (typeof en !== "string" || !/^\p{L}+$/u.test(en) || en !== en.toLowerCase()) return null;
  const length = [...en].length;
  if (length < SHARED_MIN || length > FORM_MAX) return null;
  if (!plain(it) || !Array.isArray(wrong) || wrong.length > WRONG_MAX || !wrong.every(plain)) return null;
  return { en, it, wrong };
}

/**
 * The term after a contributor of `trust` said `incoming` about the same word, given what is there:
 * `current` = {term, trust} or null. null if nothing changes. A weaker voice is ignored; the same
 * or a stronger one decides the Italian (the last of equals wins) and the one it replaces becomes
 * a wrong form; wrong forms add up and never include the Italian.
 */
export function merge(current, incoming, trust) {
  if (current && trust < current.trust) return null;
  const term = current
    ? { ...current.term, wrong: [...current.term.wrong] }
    : { en: incoming.en, it: incoming.it, wrong: [] };
  if (!same(term.it, incoming.it)) term.wrong.push(term.it);
  term.it = incoming.it;
  const kept = [];
  for (const wrong of [...term.wrong, ...incoming.wrong]) {
    if (!same(wrong, term.it) && !kept.some((k) => same(k, wrong))) kept.push(wrong);
  }
  term.wrong = kept;
  if (current && current.term.it === term.it && JSON.stringify(current.term.wrong) === JSON.stringify(term.wrong)) {
    return null;
  }
  return term;
}

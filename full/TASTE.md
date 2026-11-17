# Full Italian: taste rules

Rules for the "tutto in italiano" variant, where names (skills, traits, sigils, runes,
places, boons...) are translated too. The hybrid variant (the main DB) is not touched.
Every term decision goes in `terms.toml`; these rules say how a decision is made.

## Above every rule

A good translation comes from judgment, not from a list. Some of the things it must
weigh, as examples and not a complete list: faithful to the English, right for the
context where the text shows, good taste, logical use (the same thing named the same way,
different things kept apart). Other cases will need other reasons. The rules below are
tools, not a checklist to apply blindly: think about what each one is for before using
it, and when a rule seems to give a wrong result in a case, stop and ask (agent: report
it).

## Choosing a term

1. **Spoken test.** A term passes only if an Italian player would say it out loud in voice
   chat. A word that exists only in the dictionary is out ("Berserker fantasmatico": no;
   "Berserker fantasma", like "nave fantasma": yes).
2. **Frequency filter.** Every Italian word of a new term is looked up in the Italian
   subtitle frequency list (spoken Italian). Under ~20 occurrences it is out, unless:
   - the English word is just as rare in the English list (Alacrity 78 → Alacrità 5 is
     fine: the game picked an unusual word on purpose);
   - it is a plain formation from a common word (elemento + -ista → Elementalista).
3. **Several candidates, strict judge.** Write 4-5 candidates, drop each one for a stated
   reason: calque, rare, ambiguous, wrong sense (Spettrale = spirit of the dead), too long
   for a tooltip, sounds corporate in a fantasy world (Società commerciale). If none is
   left, the English name stays.
4. **Precedents first.** Guild Wars 1 had an official Italian translation (Ipnotizzatore,
   Negromante, Elementalista, Guerriero...). When GW1 named the same thing, start from it.
   Professional Italian localizations (WoW, Diablo, Hearthstone, LoL) are a register
   reference, not a source to copy.
   **Official GW2 translations.** When it is unclear whether a name is a proper name or
   a common word, or whether to translate it at all, compare the official German,
   French and Spanish versions: the API gives them with the same id and `?lang=de`,
   `?lang=fr`, `?lang=es` (e.g. `/v2/masteries?ids=...&lang=de`). If all three translate
   it, translate it (Mistborn Key = Nebelgeborener Schlüssel / Clé de la Brumaille /
   Llave surgida de la Niebla → "Chiave delle Nebbie"; Inner Nayos = Inneres Nayos /
   Nayos intérieur → "Nayos Interno"). If they keep it, that is a strong hint it is a
   proper name (Skiff stays in German). They show what a name means and how it was
   treated, not the Italian word to use.
5. **Context.** When a term is unclear, first find out what the string is (a trait, a
   skill, an item, a specialization, a line of dialogue) and read its description: the
   string's own text in the DB, the official API descriptions, nearby string ids,
   https://wiki.guildwars2.com/wiki/<Name>. Write what you found in the term's `context`.
   Only if a doubt is left after that, ask the user (in session) or report it (agent).
   Example: Trickery's description ("new ways to manage initiative... augments to the
   Steal skill", quote "Watch your step.") made Raggiro the pick over Astuzia/Imbroglio.
6. **One word, one meaning.** A term must not collide with another term (Might and Power
   cannot both be "Potenza"). A new term is checked against the whole of `terms.toml`.
   A clash only counts where both can be read in the same place: a specialization name
   shows only in the specialization panel, so it may share a word with a skill
   (Retribution = Castigo, Strength = Forza like the Might boon).
7. **Never translated:** personal names (Blish, Joko, Aurene, Faren) and invented words
   (Elona, Gyala, Mesmer-like coinages). In a place, the common part is translated and the
   invented part kept: Sunqua Peak → Picco Sunqua, Lion's Arch → Arco del Leone.
8. **Words that qualify a name** (stat prefixes "Berserker's"/"Carrion", "Phantasmal X",
   "Draconic", material and set words): pick the Italian form by what it says about the
   thing, not by the English part of speech. Three forms, checked in this order:
   - **"da X"**: a role or type of person, the item is the gear of that kind of person
     (GW1: "Berserker's Obsidian Helm" = "Elmo d'Ossidiana da Forsennato"; "Elmo Draconico
     da Berserker", "Spada da Cavaliere").
   - **an adjective that agrees with the noun**: only if the adjective says what the thing
     does or what character it has ("Spada Funesta" brings doom, "Elmo Rabbioso",
     "Berserker Fantasma").
   - **"del/della X"**: what the thing brings or belongs to, an animal, a force
     ("Spada del Drago", "Elmo della Vipera", Carrion = "Spada della Putrefazione").
   These checks were made for the stat prefixes, a list of qualifiers that say what the
   gear is for. Elsewhere use them with judgment, and if it is not clear they apply, ask
   the user (agent: report it, do not guess). Checks before keeping an adjective: (a) it
   must not give the object a bad state the English does not mean: "Carrion" is the set of
   rot and death, not a rotten sword, so "Spada Putrida" is out and "della Putrefazione"
   is in. When the English does mean the state, the Italian says it too: "Worn Key" =
   "Chiave Logora", "Rusty Key" = "Chiave Arrugginita"; (b) it must not be an insult or a joke in spoken Italian ("Spada della Carogna": carogna
   is an insult); (c) it must not read as a different everyday sense (Ristoratore = a
   restaurant owner); (d) try it on a feminine and a plural noun too ("Spada Salda",
   "Gambali Saldi") and on the longest real item name ("Gambali Draconici della
   Putrefazione"): if one of them sounds wrong, change the form. Two qualifiers of the same
   list must not share a word (Knight's = da Cavaliere, Cavalier's = da Cavaliere Errante;
   Mighty = Possente, Potent = Potente). Word order: the noun first, then the material/set
   adjective, then the qualifier ("Berserker's Draconic Leggings" = "Gambali Draconici da
   Berserker").
9. **The user's word is final.** Words the user rejected are in `rejected` of each term and
   must never come back for that term, in any form. A word rejected for one term may still
   be right for another (Spettro: rejected for Revenant, used for Specter).

## Using a term in a text

- **Case follows the English.** "Power" → "Potenza", "power" → "potenza"; a sentence start
  is capitalized as Italian requires. A name written in Title Case in English keeps it in
  Italian: every full word capitalized, articles and prepositions lowercase ("Phantasmal
  Berserker" → "Berserker Fantasma", "Galeshot" → "Tiratore delle Tempeste", "Legendary
  Dragon Stance" → "Posizione del Drago Leggendario"). The same name written lowercase in a
  sentence stays lowercase ("your phantasms" → "i tuoi fantasmi").
- **Forms follow the context.** A term lists its forms (name, noun, verb, adjective); the
  text takes the one its sentence needs: "Azzoppato" as the condition's name,
  "l'azzoppamento" in "la durata dell'azzoppamento", "azzoppa" for "cripples".
- **Articles and agreement** follow the Italian term's gender and number, not the English:
  "all'Arco del Leone", "la Compagnia Mercantile del Leone Nero".
- **A plain word stays plain.** A term is the mechanic only where the English means the
  mechanic: "Your alacrity is inspiring" in a dialogue is the plain word.

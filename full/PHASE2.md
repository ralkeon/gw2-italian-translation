# Full Italian: phase 2 and 3 plan

Phase 1 (done 2026-11-17): the glossary, `terms.toml` (1388 terms) and the rules, `TASTE.md`.
This plan is a proposal: nothing below is built until the owner approves it.

## What the player gets

An option in the addon: **Ibrida** (today: names of skills, traits, specializations, boons... in
English) or **Tutto in italiano** (every name in Italian, with our glossary), with a short
description. Switching is instant, no download of a second database.

## Design: one base, one overlay

The hybrid DB (`translations`) stays the base and is never rewritten for the full variant.
Next to it:

1. **`names_it`** (en, kind, it, forms, seq): the Italian of every name the hybrid keeps in
   English (`english_names`, ~4.600 entries: skills, traits, items, legendaries...) and of the
   glossary terms (`terms.toml`). It is what the addon uses instead of the keep-English list.
2. **`full`** (id, hash, it, base_seq, source, at, seq): only the rows whose text differs in the
   full variant (rows with a kept English name or term: ~9-15k). The addon in full mode reads
   `full` first, else `translations`. A row is stale when the base row changed after
   `base_seq`: it is rebuilt the next night.

Both are new tables in `data/gw2it.db`, so they also go into the nightly download as they are.

## Steps

1. **Load the glossary** on the server: `gw2it-server full-terms` reads `full/terms.toml` into
   `names_it` (kind, forms, rejected kept for the checks). Server only.
2. **Translate the names** (`english_names` -> `names_it`): the agent (`claude -p` on ifalna)
   in batches by kind (skills, traits, items...), with TASTE.md + terms.toml + the official
   de/fr/es names (`officials.db`) + the GW1 pairs; the item set words of group 3 (232 weapon
   stems) here too. Every batch: checks (case, forms, frequency of coined words, clash with a
   term in the same place), a report; the owner can veto by kind.
3. **Build the overlay**: job `gw2it-server full` (nightly): whole text = a name -> `names_it`;
   a name inside a sentence -> minimal rewrite by the model (the names job's machinery in
   reverse: English name out, Italian form in, articles and agreement fixed), with the same
   guards (markup, plural marks, no English left but the names that stay English in both
   variants: proper and invented names). The review job checks the overlay rows too.
4. **Sync**: `/sync` gets `fsince` -> overlay rows + `names_it` updates (`fseq`), only for an
   addon that asks; older addons see nothing new.
5. **Addon** (a DLL release): the option and its description, the `full` and `names_it`
   tables, lookup overlay-first in full mode, live translation of new strings with `names_it`
   instead of the keep list. README: how to switch, and the keybinding to see the English.
6. **Agent, phase 3**: nightly, like the hybrid agent: new names of a patch -> `names_it`
   (coins with TASTE rules), overlay of new rows, report in the same nightly report; the owner
   vetoes.

## Order and coordination

- Another session builds `lt_sync` in `crates/server` (lt_sync.rs, main.rs). Phase 2 server
  work starts after that is committed, in its own modules (`full.rs`, `full_terms.rs`) with
  small hooks in main.rs, one session committing at a time.
- ifalna gets `full/terms.toml` through the repo clone: the full/ commit must be pushed first.
- GW1 texts never go to GitHub: `gw1_en_it.tsv` is copied to ifalna by scp (not in git).

## Open questions for the owner

1. Push the full/ commit to GitHub (needed for ifalna to read terms.toml)?
2. Who translates the ~4.600 names in step 2: the agent with `claude -p` (owner's subscription,
   best taste, slower) or the LLM chain the names job uses (faster, needs the review pass)?
3. The addon release (step 5) is last: OK to build steps 1-4 first and test them on the
   server only?

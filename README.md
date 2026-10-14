# Italian Translation

Addon per [Nexus](https://raidcore.gg/Nexus) che traduce in italiano i testi di Guild Wars 2,
attraverso l'addon [Text Translator](https://github.com/ideka/modulep) di Ideka.

- I testi arrivano dal gioco tramite Text Translator e vengono tradotti con DeepL (la tua chiave API).
- Le traduzioni restano sul tuo PC (`strings.db`): ogni testo si traduce una volta sola.
- Ogni 2 minuti le traduzioni nuove vanno su un database globale e arrivano quelle degli altri
  giocatori: chi trova per primo un testo lo traduce per tutti, e le correzioni a mano hanno la
  precedenza su quelle automatiche (vedi [`server/`](server/)).
- I plurali degli oggetti sono generati nella sintassi del gioco (`Cuore[pl:"Cuori"] di torrone`).
- I collaboratori hanno un editor in gioco per correggere le traduzioni.

## Installazione

Serve Nexus con l'addon Text Translator. Il gioco deve essere in inglese.

1. Copia `italian_translation.dll` in `Guild Wars 2/addons/`.
2. Copia `gw2it_shim.exe` e `module.toml` in
   `Guild Wars 2/addons/text_translator/modules/italian_translation/`.
3. In gioco carica l'addon da Nexus, poi in Text Translator scegli il modulo **Italian Translation**.
4. Apri la finestra dell'addon (`ALT+SHIFT+I`) e inserisci la tua chiave DeepL in **Chiavi API**
   (basta il piano gratuito).

I dati stanno in `Guild Wars 2/addons/italian_translation/`:

| File | Cosa contiene |
|---|---|
| `config.toml` | impostazioni, chiavi API e chiavi del database globale (`[sync]`, `enabled = false` per spegnerlo) |
| `strings.db` | testi del gioco e traduzioni |
| `map_it.db` | gli stessi testi nel formato di Local Translator, da scambiare tra collaboratori |
| `contributor.json` | solo per i collaboratori: `{"user": "Nome"}` attiva l'editor |

## Editor (collaboratori)

Con `contributor.json` la finestra mostra l'editor: cerca un testo (per ID, inglese o italiano),
correggilo e salva. Le correzioni a mano non vengono mai sovrascritte e arrivano subito in gioco.
Il salvataggio è bloccato se mancano o avanzano token del gioco (`%str1%`, `%num1%`, `<c=...>`).

Plurali: subito dopo ogni parola che cambia scrivi la forma plurale, come fa il gioco:
`Broken Bottle[s]` → `Flacone[pl:"Flaconi"] rotto[pl:"rotti"]`.

## Compilare

```
cargo build --release
```

Produce `target/release/italian_translation.dll` e `target/release/gw2it_shim.exe`.
Il server del database globale è in [`crates/server`](crates/server) (in uso, su ifalna) e in
[`server/`](server/README.md) (Cloudflare: riserva automatica,
aggiornata ogni notte).

## Licenza

MIT, vedi [LICENSE](LICENSE). La generazione dei plurali (`plural.rs`) è un porting di
`plurale_it.py` di [gw2-nexus-local-translator](https://github.com/NikeGipple/gw2-nexus-local-translator) di NikeGipple.

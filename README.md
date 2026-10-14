# Italian Translation

Addon per [Nexus](https://raidcore.gg/Nexus) che traduce in italiano i testi di Guild Wars 2,
attraverso l'addon [Text Translator](https://github.com/ideka/modulep) di Ideka.

## Requisiti

- **Guild Wars 2** con la lingua del gioco in **inglese**: i testi vengono tradotti partendo
  dall'inglese.
- **[Nexus](https://raidcore.gg/Nexus)** di Raidcore, il caricatore di addon.
- **Text Translator** di Ideka, da installare con un clic dalla libreria degli addon di Nexus.

Nient'altro: la chiave DeepL è facoltativa.

## Installazione

1. Copia `italian_translation.dll` (dall'ultima release) in `Guild Wars 2/addons/`.
2. Avvia il gioco.

Al primo avvio l'addon installa da solo il suo modulo in Text Translator e lo sceglie come modulo
attivo. Se i testi restano in inglese, riavvia il gioco una volta (o scegli **Italian Translation**
tra i moduli di Text Translator). La finestra dell'addon si apre con `ALT+SHIFT+I`.

## Come funziona

- I testi arrivano dal gioco tramite Text Translator e vengono tradotti dal server del progetto,
  oppure con le tue chiavi, se le inserisci nelle opzioni dell'addon in Nexus (tutte facoltative,
  con piani gratuiti): DeepL, poi Gemini, Azure Translator e altri modelli linguistici (Groq,
  Cerebras, OpenRouter, Mistral) quando i precedenti finiscono la quota o non rispondono.
- Le traduzioni restano sul tuo PC (`strings.db`): ogni testo si traduce una volta sola.
- Ogni 2 minuti le traduzioni nuove vanno su un database globale e arrivano quelle degli altri
  giocatori: chi trova per primo un testo lo traduce per tutti, e le correzioni a mano hanno la
  precedenza su quelle automatiche (vedi [`server/`](server/)).
- I plurali degli oggetti sono generati nella sintassi del gioco (`Cuore[pl:"Cuori"] di torrone`).

I dati stanno in `Guild Wars 2/addons/italian_translation/`:

| File | Cosa contiene |
|---|---|
| `config.toml` | impostazioni, chiavi API e chiavi del database globale (`[sync]`, `enabled = false` per spegnerlo) |
| `strings.db` | testi del gioco e traduzioni |
| `map_it.db` | gli stessi testi nel formato di Local Translator, da scambiare tra collaboratori |

## Compilare

```
cargo build --release
```

Produce `target/release/italian_translation.dll`, che contiene anche il modulo per Text Translator
(`gw2it_shim.exe` e `module/module.toml`, scritti in `text_translator/modules/italian_translation/`
a ogni avvio se sono cambiati).
Il server del database globale è in [`crates/server`](crates/server) (in uso, su ifalna) e in
[`server/`](server/README.md) (Cloudflare: riserva automatica,
aggiornata ogni notte).

## Licenza

MIT, vedi [LICENSE](LICENSE). La generazione dei plurali (`plural.rs`) è un porting di
`plurale_it.py` di [gw2-nexus-local-translator](https://github.com/NikeGipple/gw2-nexus-local-translator) di NikeGipple.

# Italian Translation

Addon per [Nexus](https://raidcore.gg/Nexus) che traduce in italiano i testi di Guild Wars 2,
attraverso l'addon [Text Translator](https://github.com/ideka/modulep) di Ideka.

## Requisiti

- **Guild Wars 2** con la lingua del gioco in **inglese**: i testi vengono tradotti partendo
  dall'inglese.
- **[Nexus](https://raidcore.gg/Nexus)** di Raidcore, il caricatore di addon.
- **Text Translator** di Ideka, da installare con un clic dalla libreria degli addon di Nexus.

Le chiavi API dei traduttori (DeepL, Gemini e gli altri, anche con i piani gratuiti) sono
facoltative ma **consigliate**: senza, l'addon si appoggia al server del progetto, che ha limiti
di traduzione giornalieri molto bassi, condivisi con tutti gli altri giocatori. Si inseriscono
nelle opzioni dell'addon in Nexus.

## Installazione

1. Installa i requisiti qui sopra e avvia il gioco almeno una volta.
2. Scarica `italian_translation.dll` dall'ultima [release](https://github.com/ralkeon/gw2-italian-translation/releases/latest)
   e copiala in `Guild Wars 2/addons/`.
3. Avvia il gioco e, dalla finestra degli addon di Nexus, attiva **Text Translator** e
   **Italian Translation** (se non lo sono già).

Al primo avvio l'addon installa da solo il suo modulo in Text Translator e lo sceglie come modulo
attivo. Dopo qualche secondo (il tempo del primo scambio con il database globale, che porta le
traduzioni già fatte dagli altri) i testi compaiono in italiano. La finestra dell'addon si apre
con `ALT+SHIFT+I`.

Se i testi restano in inglese:
- riavvia il gioco una volta (o scegli **Italian Translation** tra i moduli di Text Translator);
- nelle opzioni di Text Translator controlla che **Show Translated Text** sia attivo e
  **Pause Refreshes** spento: con i refresh in pausa le nuove traduzioni si applicano solo con un
  refresh manuale. Spegnendo Show Translated Text (anche con il tasto assegnato in Nexus) il gioco
  torna in inglese, ma l'addon continua a raccogliere e tradurre i testi in background.

## Come funziona

- I testi arrivano dal gioco tramite Text Translator e vengono tradotti dal server del progetto,
  oppure con le tue chiavi, se le inserisci nelle opzioni dell'addon in Nexus (tutte facoltative,
  con piani gratuiti): DeepL, Gemini, Azure Translator e altri modelli linguistici (Groq,
  Cerebras, OpenRouter, Mistral) quando i precedenti finiscono la quota o non rispondono.
- Le traduzioni restano sul tuo PC (`strings.db`): ogni testo si traduce una volta sola.
- Ogni 2 minuti le traduzioni nuove vanno su un database globale e arrivano quelle degli altri
  giocatori: chi trova per primo un testo lo traduce per tutti. Le traduzioni fatte dal server
  hanno la precedenza su quelle automatiche degli altri addon.

Impostazioni e traduzioni stanno in `Guild Wars 2/addons/italian_translation/`
(`config.toml` e `strings.db`). Per non condividere le traduzioni con il database globale metti
`enabled = false` nella sezione `[sync]` di `config.toml`.

## Stato del progetto

Italian Translation è un progetto che porto avanti nel tempo libero. Proverò a tenerlo aggiornato
e a sistemare i problemi che mi segnalate, ma a volte ci vorrà un po' di tempo prima di una
risposta o di una nuova versione: grazie per la pazienza!

## Licenza

MIT, vedi [LICENSE](LICENSE). La generazione dei plurali (`plural.rs`) è un porting di
`plurale_it.py` di [gw2-nexus-local-translator](https://github.com/NikeGipple/gw2-nexus-local-translator) di NikeGipple.

# Italian Translation

Addon per [Nexus](https://raidcore.gg/Nexus) che traduce in italiano i testi di Guild Wars 2,
attraverso l'addon [Text Translator](https://github.com/ideka/text-translator-public) di Ideka.

> [!IMPORTANT]
> **⚠️ CON GLI ALTRI GIOCATORI USA SEMPRE I NOMI IN INGLESE**
>
> In chat, nelle richieste di aiuto, nelle discussioni, nelle segnalazioni e quando cerchi su wiki
> o guide, **non usare i nomi tradotti**: la maggior parte dei giocatori, anche italiani, conosce
> solo quelli inglesi, e wiki e guide sono in inglese.
>
> **Assegna in Nexus un tasto a Show Translated Text** (Text Translator): premilo per vedere al volo
> il nome originale in inglese, ripremilo per tornare all'italiano. Vale per entrambe le varianti
> (ibrida e tutto in italiano); l'addon intanto continua a ricevere le traduzioni.

> [!WARNING]
> **Testi che non si aggiornano?** Se un testo resta in inglese anche dopo che è stato tradotto, o se
> premendo il tasto di Show Translated Text alcuni testi non tornano in inglese (o non tornano in
> italiano), spegni **Pause Refreshes**: Nexus → opzioni dell'addon **Text Translator** (di Ideka)
> → casella **Pause Refreshes**. Con i refresh in pausa il gioco non ridisegna i testi già a
> schermo, che quindi non cambiano finché non li riapri.

## Disclaimer

**EN** — Italian Translation is an unofficial, fan-made project. It is not affiliated with or
endorsed by ArenaNet, NCSoft or Guild Wars 2. Guild Wars 2 and its texts belong to their owners.
It runs as a Nexus addon on top of Text Translator and does not modify any game file. The
translations are produced automatically by AI translation agents and may contain errors. Use at
your own risk.

*To ArenaNet:* if you see anything here that is a problem and should be removed or changed,
please contact me (an issue on this repository is fine) and I will take care of it.

**IT** — Italian Translation è un progetto amatoriale non ufficiale. Non è affiliato né approvato
da ArenaNet, NCSoft o Guild Wars 2. Guild Wars 2 e i suoi testi appartengono ai rispettivi
proprietari. Funziona come addon di Nexus sopra Text Translator e non modifica nessun file del
gioco. Le traduzioni sono prodotte automaticamente da agenti di traduzione basati su intelligenza
artificiale e possono contenere errori. Usalo a tuo rischio.

*Ad ArenaNet:* se vedete qualcosa che non va bene e che deve essere rimosso o modificato,
contattatemi (basta aprire una issue su questo repository) e me ne occuperò io.

## Requisiti

- **Guild Wars 2** con la lingua del gioco in **inglese**: i testi vengono tradotti partendo
  dall'inglese.
- **[Nexus](https://raidcore.gg/Nexus)** di Raidcore, il caricatore di addon.
- **[Text Translator](https://github.com/ideka/text-translator-public)** di Ideka, da installare con un clic dalla libreria degli addon di Nexus.

Non servono chiavi né account.

## Installazione

1. Installa i requisiti qui sopra e avvia il gioco almeno una volta.
2. Scarica `italian_translation.dll` dall'ultima [release](https://github.com/ralkeon/gw2-italian-translation/releases/latest)
   e copiala in `Guild Wars 2/addons/`.
3. Avvia il gioco e, dalla finestra degli addon di Nexus, attiva **Text Translator** e
   **Italian Translation** (se non lo sono già).

Al primo avvio l'addon installa da solo il suo modulo in Text Translator e lo sceglie come modulo
attivo. Dopo qualche secondo, il tempo di collegarsi al server, i testi compaiono in italiano. La finestra dell'addon si apre con
`ALT+SHIFT+I`.

Se i testi restano in inglese:
- riavvia il gioco una volta (o scegli **Italian Translation** tra i moduli di Text Translator);
- nelle opzioni di Text Translator controlla che **Show Translated Text** sia attivo e
  **Pause Refreshes** spento: con i refresh in pausa le nuove traduzioni si applicano solo con un
  refresh manuale. Spegnendo Show Translated Text (anche con il tasto assegnato in Nexus) il gioco
  torna in inglese, ma l'addon continua a ricevere le traduzioni in background.

## Come funziona

L'addon si collega al server del progetto, dove vengono tradotti i testi del gioco, e ti porta le
traduzioni dei testi che incontri. Un testo non ancora tradotto resta in inglese finché la sua
traduzione non arriva; quando arriva compare da sola, senza riavviare.

### Ibrida o tutto in italiano

La traduzione può essere ibrida o completamente in italiano. Per attivare la versione tutta in
italiano: Nexus → opzioni di **Italian Translation** → casella **Tutto in italiano**.
- **spenta** (traduzione ibrida, la scelta di partenza): i nomi di abilità, tratti,
  specializzazioni, luoghi, fazioni, boon e condition restano in inglese, come li dicono di solito
  i giocatori italiani e come li trovi nelle guide e sulla wiki;
- **accesa**: anche quelli in italiano (Berserker Fantasma, Arco del Leone, Forza, Legione delle
  Ceneri), con un glossario scelto apposta e le traduzioni ufficiali di Guild Wars 1 dove esistono.
  La variante si completa giorno dopo giorno: i testi non ancora pronti si vedono come
  nell'ibrida.

Il cambio vale subito, senza riavviare.

Impostazioni e traduzioni stanno in `Guild Wars 2/addons/italian_translation/`.

## Stato del progetto

Italian Translation è un progetto che porto avanti nel tempo libero. Proverò a tenerlo aggiornato
e a sistemare i problemi che mi segnalate, ma a volte ci vorrà un po' di tempo prima di una
risposta o di una nuova versione: grazie per la pazienza!

## Licenza

MIT, vedi [LICENSE](LICENSE). La generazione dei plurali (`plural.rs`) è un porting di
`plurale_it.py` di [gw2-nexus-local-translator](https://github.com/NikeGipple/gw2-nexus-local-translator) di NikeGipple.

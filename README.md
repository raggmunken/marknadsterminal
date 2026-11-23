# Marknadsterminalen

Kod till projektplanen "Egen marknadsterminal i Rust". Det här är kedjemotorn
från dokument till granskad relation, med lagring, API, gränssnitt och
grunden för signalmotorn. Allt nedan är kört på ett påhittat testdokument.
Ingenting är kört mot riktiga källor än, eftersom det kräver uppgifterna i
`.env.example`.

## Flödet

```
dokument -> text -> stycken -> extraktion -> citatkontroll -> bolagsmatchning
   steg 1            steg 2      steg 3         steg 4            steg 5

-> omvänd sökning -> sammanslagning -> granskningskö -> kedjevy och API
      steg 6             steg 7           steg 8
```

## Paketen

| Paket | Ansvar | Läge |
|---|---|---|
| `term-core` | Gemensamma typer | Testat |
| `term-ingest` | EDGAR-klient, takthållare, fulltextsökning | Takthållare och tolkning testade. Aldrig körd mot EDGAR. Sökningens svarsformat är ett antagande |
| `term-parse` | HTML till text, urval av stycken | Testat på påhittad text |
| `term-extract` | Extraktion, citatkontroll, facit och mått | Regelbaserad extraktor körd. Språkmodellen skriven, aldrig körd skarpt |
| `term-resolve` | Bolagsmatchning, spegling, sammanslagning | Testat på påhittade bolag |
| `term-store` | Lagring i en SQLite-fil | Testat, även mot fil på disk |
| `term-api` | HTTP-gränssnitt | Testat, och kört som server på localhost |
| `term-signal` | Signalkort, låst logg, fyra grindar, statistik | Testat. Innehåller ingen regel som ger signaler |
| `term-ui` | Fönstret: KEDJA, BEVIS, GRANSKA | Testat utan fönster. Ingen har sett det |
| `term-cli` | Kommandot `term` | Kört från början till slut |

## Kom igång

```sh
cargo test                 # allt utom gränssnittet, snabbt
cargo test --workspace     # allt, tar några minuter första gången
cargo clippy --workspace --all-targets -- -D warnings
```

Prova hela flödet på testdokumentet:

```sh
cargo run -p term-cli -- companies add 42 "Nordvik Test Components Inc." --ticker NTC
cargo run -p term-cli -- ingest fixtures/syntetisk_10k.html --cik 42
cargo run -p term-cli -- review list
cargo run -p term-cli -- review approve 1 --note "kontrollerad mot dokumentet"
cargo run -p term-cli -- show NTC --all
cargo run -p term-ui          # skriv NTC KEDJA i kommandoraden
cargo run -p term-cli -- serve   # http://127.0.0.1:8787/companies/42/chain?status=all
```

## Mot riktiga källor

Kopiera `.env.example` till `.env` och fyll i. Kommandot `term` läser filen själv.

```sh
# Bolagslistan som namn matchas mot. Hämta SEC:s fil company_tickers.json först.
cargo run -p term-cli -- companies import company_tickers.json

# Hämta senaste 10-K och läs in den. Kommandot skriver ut nästa rad åt dig.
cargo run -p term-cli -- fetch 1467858 --out gm_10k.htm
cargo run -p term-cli -- ingest gm_10k.htm --cik 1467858 --extractor anthropic

# Vilka andra bolag nämner bolaget?
cargo run -p term-cli -- reverse 1467858
```

Inget i det här blocket har körts.

## Regler i koden

- **Citatet avgör.** En relation släpps bara igenom om citatet finns i stycket
  som extraktorn fick se. Bara blanksteg och citattecken får skilja.
- **Osäker matchning tillämpas aldrig.** Bara exakt namnlikhet sätter ett CIK.
  Allt annat visas som förslag.
- **Ingenting är godkänt förrän en människa har sagt det.** API:et och
  `term show` visar bara godkända relationer om man inte ber om annat.
- **Ändrat belägg kräver ny granskning.** En relation som får nytt innehåll går
  tillbaka till kön. En ändrad stavning av namnet gör det inte.
- **Signalloggen går inte att ändra obemärkt.** Varje rad bär en kontrollsumma av
  raden före. `term signal verify-log` kontrollerar kedjan.
- **En regel kan inte hoppa över en grind.** Gränsen t över 3,0 kommer ur
  forskningen i planen. 28 veckor och 30 observationer är planens egna tumregler.
- **Facit ägs av människor.** `fixtures/*.facit.json` ändras aldrig av en extraktor.
- **Kört eller inte sant.** Varje modul säger i sin rubrik vad som är kört.

## Vad som inte finns

- Ingen regel som ger signaler. De tre signaltyperna kräver kurshistorik,
  estimat och händelsedata som projektet inte har.
- Ingen kursdata, inga flöden, inga händelser, ingen prognosmodell. Det är
  planens del 2 och större delen av del 3.
- Ingen inloggning i API:et. Det lyssnar på localhost.
- Vyerna FILINGS, PEERS, FIN, AGARE, INSIDER, LEDNING, KARTA och KURS.

## Avvikelser från planen

- **SQLite i stället för PostgreSQL.** Prototypen ska gå att köra utan server.
  Schemat är vanlig SQL. Beslut om byte tas i fas 0.
- **Språkmodellen anropas direkt över HTTP**, inte genom `rig-core`.
- **HTML rensas med en egen funktion**, inte med ett tolkbibliotek.
- **Nivåerna Disclosed, Named och Reverse** har prototypens tolkning. Se `term-core`.
- **Gränssnittet läser databasen direkt**, inte genom API:et. Det räcker för en
  användare på en dator.

## Versioner som måste hållas ihop

`eframe` 0.36, `egui_dock` 0.21 och `egui_graphs` 0.32 kräver alla `egui` 0.36.
Senaste `egui_dock` kräver redan 0.37. Uppgradera de tre tillsammans eller inte alls.

Nästa uppgifter står i `uppgifter/nasta.yaml`.

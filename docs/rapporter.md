# Rapporter som namnger kunder och leverantörer

Det här är en karta över dokument som bolag enligt lag, börsregler eller bokföringsstandard måste lämna, och som kan avslöja vilka de säljer till eller köper från. Den är skriven för att styra vad terminalen läser, bolag för bolag och land för land.

**Läs detta först.** Rättsliga hänvisningar är gjorda ur minnet av författaren och inte kontrollerade mot lagtexten. De ska ses som sökvägar till rätt regelverk, inte som juridisk rådgivning. Kolumnen "Namnger motparter?" är det som spelar roll för terminalen: många regelverk kräver bara att *att* ett beroende finns, inte *vem* det är.

Status: **byggt** betyder att terminalen hämtar och läser dokumentet idag. **nyckel** betyder att det går att bygga men kräver en API-nyckel. **ej byggt** betyder att det inte finns kod än.

## USA (SEC, gratis, byggt)

| Dokument | Rättslig grund | Namnger motparter? | Status |
|---|---|---|---|
| Årsredovisning 10-K | Securities Exchange Act, Regulation S-K Item 101 (verksamhet, beroende av få kunder), Item 105 (riskfaktorer), Item 303 | Ofta, när beroendet är väsentligt, ofta med procentsats. Råvaruleverantörer namnges sällan | byggt |
| Väsentliga avtal, bilagor EX-10 | Regulation S-K Item 601(b)(10) | Ja, parterna står i avtalet (delar kan vara svärtade) | byggt (heuristik på filnamn) |
| 8-K punkt 1.01 och 1.02 | Form 8-K: ingående och uppsägning av väsentliga avtal | Ja, motparten namnges | byggt |
| 8-K pressmeddelanden EX-99 | Frivilligt eller punkt 7.01 och 8.01 | Ofta ("utsedd till leverantör till X") | byggt |
| Kvartalsrapport 10-Q | Samma regelverk, kortare | Ibland, i koncentrationsnoter | byggt |
| Prospekt S-1, 424B4, 424B5 | Securities Act | Ja i S-1, med kundkoncentration och leveransavtal i detalj | byggt |
| Fullmakt DEF 14A | Regulation S-K Item 404: transaktioner med närstående | Ja, när en närstående säljer till eller köper från bolaget | byggt |
| Mineralrapport, blankett SD | Exchange Act Rule 13p-1 (konfliktmineraler) | Ja, för smältverk och ibland leverantörer | byggt |
| Utländska bolags årsredovisning 20-F, 40-F, 6-K | Exchange Act, för utländska emittenter | Som 10-K, ofta mer allmänt | byggt |
| Beloppsnot om stora kunder i bokslutet | US GAAP ASC 280 och ASC 275 | Procent, namn behövs inte | läses som del av 10-K |
| Statliga kontrakt, USAspending | FFATA (statlig utgiftsrapportering) | Ja, både huvudentreprenör och underleverantör | byggt (egen källa) |

## EU och Storbritannien

| Dokument | Rättslig grund | Namnger motparter? | Status |
|---|---|---|---|
| Hållbarhetsrapport enligt CSRD och ESRS, inkl. värdekedjan | Direktiv (EU) 2022/2464 | Mest kategorier och risker, sällan namn. Esef-formatet gör filerna maskinläsbara | delvis (ESEF-register via filings.xbrl.org) |
| Offentliga upphandlingar, kontraktstilldelningar (TED) | Direktiv 2014/24/EU med flera | Ja, vinnande leverantör och värde | ej byggt, gratis öppna data |
| Aktsamhetsrapport enligt tyska leverantörskedjelagen | Lieferkettensorgfaltspflichtengesetz (LkSG) | Oftast riskområden och inte namn | ej byggt |
| Aktsamhetsplan, Frankrike | Loi de vigilance (2017) | Sällan namn | ej byggt |
| Redogörelse enligt Åpenhetsloven, Norge | Åpenhetsloven (2022) | Ibland leverantörslistor | ej byggt |
| Modern Slavery Statement, Storbritannien | Modern Slavery Act 2015, avsnitt 54 | Beskriver kedjan, namn är frivilliga | ej byggt, statligt register finns |
| Konfliktmineraler, EU | Förordning (EU) 2017/821 | Smältverk och raffinaderier | ej byggt |
| Batteriförordningen | Förordning (EU) 2023/1542 | Aktsamhetspolicy och leverantörskedja för batterier | ej byggt |
| Avskogningsförordningen | Förordning (EU) 2023/1115 | Leverantörer av råvaror (soja, kakao, trä) i aktsamhetsförklaringar | ej byggt |
| Årsredovisningar nationellt (Bolagsverket m.fl.) | Årsredovisningslagen, nationella register | Närståendetransaktioner, sällan kunder | ej byggt |
| Närståendeupplysningar | IFRS, IAS 24 | Ja, för närstående | läses via ESEF när det är byggt |
| Segmentnot med stora kunder | IFRS 8 | Procent, namn behövs inte | ej byggt |

## Asien och övriga världen

| Dokument | Rättslig grund | Namnger motparter? | Status |
|---|---|---|---|
| Japan: värdepappersrapport (yuho) | Finansinstrumentslagen (FIEA) | Ofta ja, med huvudsakliga kunder och leverantörer | nyckel (EDINET) |
| Sydkorea: affärsrapport via DART | Kapitalmarknadslagen | Ofta ja, "huvudsakliga köpare och säljare" | nyckel (Open DART) |
| Kina och Hongkong: årsrapport, prospekt | CSRC-regler och Hongkongbörsens listningsregler | Fem största kunder och leverantörer, ofta anonymiserade, namn i prospekt | ej byggt (AKShare) |
| Taiwan: MOPS | Börsens regler | Större kunder och leverantörer | ej byggt |
| Indien: BRSR | SEBI:s regler | Värdekedja, ofta aggregerat | ej byggt |
| Australien: Modern Slavery Statement | Modern Slavery Act 2018 (Cth) | Beskrivning av kedjan, offentligt register | ej byggt |
| Kanada: rapport om tvångsarbete i leverantörskedjor | Fighting Against Forced Labour and Child Labour in Supply Chains Act | Beskrivning, sällan namn | ej byggt |
| Kalifornien: leverantörskedjeupplysning | California Transparency in Supply Chains Act | Beskrivning på bolagets webbplats | ej byggt |
| Kanada och USA: tullmyndigheter | Importuppgifter, sjöfraktsedlar | Ja, men köps ofta av dataföretag | ej byggt, betalt |

## Tolkning

Ingen enskild rapport ger hela kedjan. Det som går att säga med hjälp av dem tillsammans:

1. **Beloppsbelägg finns för stora beroenden.** USA och Japan/Korea ger namn och procent. Det är därför kedjan ser tätare ut för fordon och försvar än för konsumentvaror.
2. **Hållbarhetsrapporter beskriver mest hur, inte vem.** De blir användbara först när bolaget frivilligt publicerar leverantörslistor.
3. **Offentlig upphandling är den bäst täckta källan i EU.** Den namnger leverantören och beloppet, men gäller bara det offentliga.
4. **Det som aldrig står öppet** är bolagens vanliga inköp under de beloppsgränser som lagen sätter. Där hjälper bara tullregister och bolagens egna listor.

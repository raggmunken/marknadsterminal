//! Command line entry point.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use term_core::{Cik, Company, Direction, ReviewStatus, StoredRelation, Unit};
use term_extract::anthropic::AnthropicExtractor;
use term_extract::baseline::BaselineExtractor;
use term_extract::chain::{self, ChainOutput, SourceRef};
use term_extract::eval::{evaluate, FacitRow};
use term_ingest::search::mentioning_companies;
use term_ingest::{document_url, latest_of_form, EdgarClient};
use term_parse::{html_to_text, select_passages, DEFAULT_MIN_SCORE};
use term_core::time::unix_to_iso_minutes;
use term_resolve::{merge, Directory, Resolution};
use term_analysis::{analyze_company, Level};
use term_core::names::normalize_name;
use term_core::Location;
use term_ingest::facts;
use term_sources::world;
use term_sources::{ecb, epa_frs, esef, fred, gdacs, gleif, ofac, openfigi, riksbank, usaspending, usgs, worldbank, Http};
use term_store::{now_unix, Store, Upsert};

#[derive(Parser)]
#[command(name = "term", about = "Marknadsterminalen")]
struct Cli {
    /// Databasfilen. Skapas om den saknas
    #[arg(long, global = true, env = "TERM_DB", default_value = "terminal.db")]
    db: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Visa vilka stycken i ett dokument som skulle skickas till extraktorn
    Passages {
        /// En fil med HTML (.htm, .html) eller ren text
        file: PathBuf,
        #[arg(long, default_value_t = DEFAULT_MIN_SCORE)]
        min_score: u32,
    },
    /// Kör steg 2 till 4 på en fil och skriv resultatet som JSON. Sparar ingenting
    Chain {
        file: PathBuf,
        #[arg(long)]
        cik: Cik,
        #[arg(long, value_enum, default_value_t = ExtractorKind::Baseline)]
        extractor: ExtractorKind,
        #[arg(long, default_value_t = DEFAULT_MIN_SCORE)]
        min_score: u32,
        /// Facit i JSON. Ger precision och täckning på stderr
        #[arg(long)]
        facit: Option<PathBuf>,
    },
    /// Kör steg 2 till 7 på en fil och lägg relationerna i granskningskön
    Ingest {
        file: PathBuf,
        #[arg(long)]
        cik: Cik,
        /// Bolagets namn. Läggs i bolagslistan om det anges
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long, value_enum, default_value_t = ExtractorKind::Baseline)]
        extractor: ExtractorKind,
        #[arg(long, default_value_t = DEFAULT_MIN_SCORE)]
        min_score: u32,
        /// Dokumentets accessionsnummer hos SEC
        #[arg(long)]
        accession: Option<String>,
        #[arg(long)]
        url: Option<String>,
    },
    /// Hämta bolagets senaste 10-K från EDGAR. Kräver TERM_USER_AGENT
    Fetch {
        cik: Cik,
        #[arg(long)]
        out: PathBuf,
    },
    /// Steg 6: vilka andra bolag nämner det här bolaget? Kräver TERM_USER_AGENT
    Reverse {
        target: String,
        /// Hämta och läs in årsredovisningen för så här många av dem
        #[arg(long, default_value_t = 0)]
        ingest: usize,
        #[arg(long, value_enum, default_value_t = ExtractorKind::Baseline)]
        extractor: ExtractorKind,
    },
    /// Följ kedjan nedåt: leverantörerna, deras leverantörer och så vidare. Kräver TERM_USER_AGENT
    Crawl {
        target: String,
        /// Hur många led ned
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Hur många bolag per led som följs vidare (starkast belägg först)
        #[arg(long, default_value_t = 6)]
        per_level: usize,
        /// Hur många av bolagen som nämner ett bolag som läses per bolag
        #[arg(long, default_value_t = 60)]
        reverse_cap: usize,
        #[arg(long, value_enum, default_value_t = ExtractorKind::Baseline)]
        extractor: ExtractorKind,
    },
    /// Läs in en lista med namngivna leverantörer (till exempel ett pris från kunden). Ingen siffra, svagare än kedjan
    Named {
        /// Bolaget som listan gäller (ticker eller CIK)
        target: String,
        /// Textfil med ett namn per rad
        file: std::path::PathBuf,
        /// Adressen där listan står
        #[arg(long)]
        url: String,
    },
    /// Bolagslistan som namn matchas mot
    Companies {
        #[command(subcommand)]
        action: CompaniesAction,
    },
    /// Visa ett bolags kedja. Ange CIK eller ticker
    Show {
        target: String,
        /// Visa även relationer som inte är godkända
        #[arg(long)]
        all: bool,
    },
    /// Steg 8: mänsklig granskning
    Review {
        #[command(subcommand)]
        action: ReviewAction,
    },
    /// Starta API-servern
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: SocketAddr,
    },
    /// Signalloggen
    Signal {
        #[command(subcommand)]
        action: SignalAction,
    },
    /// Händelser som kan störa en kedja: jordbävningar, cykloner, översvämningar
    Events {
        #[command(subcommand)]
        action: EventsAction,
    },
    /// Tidsserier: valuta, räntor och makro
    Series {
        #[command(subcommand)]
        action: SeriesAction,
    },
    /// Slå upp identiteter hos GLEIF (LEI) och OpenFIGI (FIGI)
    Lookup {
        #[command(subcommand)]
        action: LookupAction,
    },
    /// OFAC:s sanktionslista. Ett hjälpmedel, ingen regelefterlevnadskontroll
    Sanctions {
        #[command(subcommand)]
        action: SanctionsAction,
    },
    /// Räkenskaper ur SEC: intäkter, resultat, tillgångar, skulder, eget kapital. Kräver TERM_USER_AGENT
    Fin { target: String },
    /// Bolagets senaste insideranmälningar (blankett 4) hos SEC. Kräver TERM_USER_AGENT
    Insiders { target: String },
    /// Världen: vad källorna har för varje land
    World {
        #[command(subcommand)]
        action: WorldAction,
    },
    /// Väg ihop allt som är känt om ett bolag: kedja, granskning, sanktioner, platser, händelser
    Analyze { target: String },
    /// Platser som hör till bolag, så att händelser kan kopplas till kedjan
    Locations {
        #[command(subcommand)]
        action: LocationsAction,
    },
    /// Hämta bolagets federala underkontrakt hos USAspending. Varje huvudleverantör blir en kund
    Subawards {
        target: String,
        /// Första dag, ÅÅÅÅ-MM-DD. Standard är ett år bakåt
        #[arg(long)]
        from: Option<String>,
    },
    /// Senaste europeiska årsredovisningar (ESEF) för ett land, till exempel SE
    Esef {
        country: String,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum WorldAction {
    /// Hämta landlistan och fråga källorna land för land. Tar flera minuter
    Build {
        /// Fråga bara om de N största ekonomierna
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Visa länderna, största ekonomin först
    List {
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// Skriv hela listan som en Markdown-fil
    Export { file: PathBuf },
}

#[derive(Subcommand)]
enum LocationsAction {
    /// Sök anläggningar i USA hos EPA. Ange vems de är och vad som ska sökas
    FetchEpa {
        /// Bolaget som platserna ska höra till: CIK, ticker eller ett motpartsnamn
        owner: String,
        /// Sökord i anläggningens namn. Standard är bolagsnamnet utan bolagsform
        #[arg(long)]
        search: Option<String>,
        /// Delstat, två bokstäver
        #[arg(long)]
        state: Option<String>,
    },
    /// Lägg till en plats för hand
    Add {
        owner: String,
        lat: f64,
        lon: f64,
        #[arg(long)]
        label: String,
    },
    List {
        owner: Option<String>,
    },
}

#[derive(Subcommand)]
enum EventsAction {
    /// Hämta nya händelser och spara dem
    Fetch {
        #[arg(value_enum, default_value_t = EventSource::All)]
        source: EventSource,
        /// USGS-flöde: 4.5_day, 4.5_week, 4.5_month, significant_day, significant_week, significant_month
        #[arg(long, default_value = "4.5_day")]
        feed: String,
    },
    /// Visa sparade händelser, nyast först
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum EventSource {
    Usgs,
    Gdacs,
    All,
}

#[derive(Subcommand)]
enum SeriesAction {
    /// Hämta en serie och spara den
    Fetch {
        #[arg(value_enum)]
        source: SeriesSource,
        /// ecb: valutakod (SEK). riksbank: serienamn (SEKUSDPMI). worldbank: indikator (NY.GDP.MKTP.CD). fred: serienamn (DGS10)
        id: String,
        /// ecb: antal senaste observationer
        #[arg(long, default_value_t = 30)]
        last: u32,
        /// riksbank: första dag, ÅÅÅÅ-MM-DD. Standard är 30 dagar bakåt
        #[arg(long)]
        from: Option<String>,
        /// riksbank: sista dag. Standard är i dag
        #[arg(long)]
        to: Option<String>,
        /// worldbank: landskod
        #[arg(long, default_value = "SE")]
        country: String,
    },
    /// Visa de senaste värdena i en sparad serie
    Show {
        source: String,
        series: String,
        #[arg(long, default_value_t = 10)]
        last: usize,
    },
    /// Vilka serier finns sparade?
    List,
}

#[derive(Clone, Copy, ValueEnum)]
enum SeriesSource {
    Ecb,
    Riksbank,
    Worldbank,
    /// Kräver FRED_API_KEY
    Fred,
}

#[derive(Subcommand)]
enum LookupAction {
    /// Sök LEI på bolagsnamn
    Lei { name: String },
    /// Sök FIGI på ticker
    Figi {
        ticker: String,
        /// OpenFIGI:s börskod, till exempel US eller SS (Stockholm)
        #[arg(long, default_value = "US")]
        exch: String,
    },
}

#[derive(Subcommand)]
enum SanctionsAction {
    /// Hämta listan och ersätt den sparade kopian
    Fetch,
    /// Finns namnet som huvudnamn på listan?
    Check { name: String },
    /// Jämför alla motparter i kedjorna mot listan
    Screen,
}

#[derive(Subcommand)]
enum CompaniesAction {
    /// Läs in SEC:s fil company_tickers.json
    Import { file: PathBuf },
    /// Lägg till eller ändra ett bolag
    Add {
        cik: Cik,
        name: String,
        #[arg(long)]
        ticker: Option<String>,
    },
    List,
}

#[derive(Subcommand)]
enum ReviewAction {
    /// Relationer som väntar på beslut
    List,
    Approve {
        id: i64,
        #[arg(long)]
        note: Option<String>,
    },
    Reject {
        id: i64,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
enum SignalAction {
    /// Kontrollera att ingen rad i signalloggen har ändrats
    VerifyLog,
}

#[derive(Clone, Copy, ValueEnum)]
enum ExtractorKind {
    /// Regler utan språkmodell
    Baseline,
    /// Anthropic Messages API. Kräver ANTHROPIC_API_KEY
    Anthropic,
    /// Programmet `claude` (Claude Code) i terminalen, standard Haiku 5.5. Ingen API-nyckel, men inloggad
    ClaudeCli,
    /// Valfri tjänst med OpenAI-format. Läser TERM_LLM_BASE_URL, TERM_LLM_API_KEY och TERM_LLM_MODEL
    OpenaiCompat,
}

fn read_as_text(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("kunde inte läsa {}", path.display()))?;
    let is_html = matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("htm" | "html")
    );
    Ok(if is_html { html_to_text(&raw) } else { raw })
}

async fn run_chain(
    kind: ExtractorKind,
    cik: Cik,
    text: &str,
    source: &SourceRef,
    min_score: u32,
) -> Result<ChainOutput> {
    Ok(match kind {
        ExtractorKind::Baseline => chain::run(cik, text, source, &BaselineExtractor, min_score).await?,
        ExtractorKind::Anthropic => {
            let llm = AnthropicExtractor::from_env()?;
            chain::run(cik, text, source, &llm, min_score).await?
        }
        ExtractorKind::OpenaiCompat => {
            let llm = term_extract::openai_compat::OpenAiCompatExtractor::from_env()?;
            chain::run(cik, text, source, &llm, min_score).await?
        }
        ExtractorKind::ClaudeCli => {
            let llm = term_extract::claude_cli::ClaudeCliExtractor::from_env();
            chain::run(cik, text, source, &llm, min_score).await?
        }
    })
}

fn edgar_client() -> Result<EdgarClient> {
    let Ok(user_agent) = std::env::var("TERM_USER_AGENT") else {
        bail!("sätt TERM_USER_AGENT till vem ni är och en kontaktadress, till exempel \"Bolaget AB anna@bolaget.se\"");
    };
    Ok(EdgarClient::new(&user_agent)?)
}

/// Read a company's own latest 10-K and store the relations it states.
async fn forward_pass(store: &Store, client: &EdgarClient, directory: &Directory, cik: Cik, kind: ExtractorKind) -> Result<usize> {
    let filings = client.filings(cik).await?;
    let Some(filing) = latest_of_form(&filings, "10-K") else { return Ok(0) };
    let url = document_url(cik, filing);
    let text = html_to_text(&client.get_text(&url).await?);
    let source = SourceRef { accession: Some(filing.accession.clone()), url: Some(url) };
    let out = run_chain(kind, cik, &text, &source, DEFAULT_MIN_SCORE).await?;
    let mut n = 0;
    for mut relation in out.accepted {
        directory.apply(&mut relation);
        store.upsert_relation(&relation)?;
        n += 1;
    }
    Ok(n)
}

/// Find the filers that name the company as a customer, read them, and store what they
/// say about it. Returns the number of relations found from the other side.
async fn reverse_pass(store: &Store, client: &EdgarClient, cik: Cik, ingest: usize, verbose: bool, kind: ExtractorKind) -> Result<usize> {
    let Some(company) = store.company(cik)? else {
        bail!("CIK {cik} finns inte i bolagslistan. Lägg till det med: term companies add");
    };
    // The short name together with "accounted for" finds the filings where
    // someone reports the company as a customer. The full legal name alone
    // mostly finds banks and funds that hold its bonds.
    let short = normalize_name(&company.name);
    let today = unix_to_iso_minutes(now_unix())[..10].to_string();
    let from = unix_to_iso_minutes(now_unix() - 540 * 86_400)[..10].to_string();
    let hits = client
        .full_text_search_all(&[short.as_str(), "accounted for"], &["10-K"], Some((&from, &today)), 10)
        .await?;
    // Filers that are not in the company list (no ticker) are added from
    // the name the search result gives them, so that they can be read.
    for h in &hits {
        for (i, c) in h.ciks.iter().enumerate() {
            if *c != cik && store.company(*c)?.is_none() {
                if let Some(raw) = h.display_names.get(i) {
                    let name = raw.split("  (").next().unwrap_or(raw).trim().to_string();
                    if !name.is_empty() {
                        store.upsert_company(&Company { cik: *c, name, ticker: None })?;
                    }
                }
            }
        }
    }
    let others = mentioning_companies(&hits, cik);
    if verbose {
        println!("{} träffar. {} andra bolag nämner {short:?} tillsammans med \"accounted for\":", hits.len(), others.len());
        for other in &others {
            let name = store.company(*other)?.map(|c| c.name).unwrap_or_else(|| "okänt bolag".into());
            println!("  CIK {other}  {name}");
        }
    }
    if ingest == 0 {
        if verbose {
            println!("Läs in deras årsredovisningar med: term reverse {} --ingest 10", cik.unpadded());
        }
        return Ok(0);
    }

    let directory = Directory::new(store.companies()?);
    let (mut read, mut found) = (0, 0);
    for other in others.into_iter().take(ingest) {
        let Some(other_company) = store.company(other)? else { continue };
        let Ok(filings) = client.filings(other).await else { continue };
        let Some(filing) = latest_of_form(&filings, "10-K") else { continue };
        let url = document_url(other, filing);
        let Ok(html) = client.get_text(&url).await else { continue };
        let text = html_to_text(&html);
        let source = SourceRef { accession: Some(filing.accession.clone()), url: Some(url) };
        let out = run_chain(kind, other, &text, &source, DEFAULT_MIN_SCORE).await?;
        read += 1;
        for mut relation in out.accepted {
            directory.apply(&mut relation);
            // The filing was found by searching for our company. If it names
            // the company by its short name or its ticker, take that as our
            // company. The relation still goes to a person for review.
            let name = relation.counterparty.name.as_str();
            if relation.counterparty.cik.is_none()
                && (normalize_name(name) == short || company.ticker.as_deref() == Some(name))
            {
                relation.counterparty.cik = Some(cik);
            }
            store.upsert_relation(&relation)?;
            // Step 6: what the other company says about ours, seen from our side.
            if relation.counterparty.cik == Some(cik) {
                if let Some(mirrored) = term_resolve::mirror(&relation, &other_company.name) {
                    store.upsert_relation(&mirrored)?;
                    found += 1;
                    if verbose {
                        println!("  {} uppger {} som {}", other_company.name, company.name, match relation.direction {
                            Direction::Customer => "kund",
                            Direction::Supplier => "leverantör",
                        });
                    }
                }
            }
        }
    }
    if verbose {
        println!("{read} årsredovisningar lästa med den regelbaserade extraktorn.");
    }
    Ok(found)
}

fn strength(r: &StoredRelation) -> (u8, i64) {
    use term_core::Confidence::*;
    let rank = match r.relation.confidence {
        Disclosed => 0,
        Reverse => 1,
        Registry => 2,
        Named => 3,
    };
    let size = r.relation.amount.as_ref().map(|a| -(a.value * 100.0) as i64).unwrap_or(0);
    (rank, size)
}

/// Breadth first walk down the supply chain. Each company is read once, from its own
/// filing (forward) and from the filings of those who name it as a customer (reverse).
async fn crawl(store: &Store, client: &EdgarClient, root: Cik, depth: usize, per_level: usize, reverse_cap: usize, kind: ExtractorKind) -> Result<()> {
    let mut visited = std::collections::HashSet::new();
    let mut level = vec![root];
    visited.insert(root);
    for d in 0..depth {
        let mut next = Vec::new();
        for cik in &level {
            let name = store.company(*cik)?.map(|c| c.name).unwrap_or_else(|| cik.to_string());
            let directory = Directory::new(store.companies()?);
            let forward = forward_pass(store, client, &directory, *cik, kind).await.unwrap_or_else(|e| {
                eprintln!("  fel vid läsning av {name}: {e}");
                0
            });
            let reverse = reverse_pass(store, client, *cik, reverse_cap, false, kind).await.unwrap_or_else(|e| {
                eprintln!("  fel vid omvänd sökning för {name}: {e}");
                0
            });
            let mut suppliers: Vec<StoredRelation> = store.chain(*cik, None)?.suppliers;
            suppliers.sort_by_key(strength);
            println!("{}{} (led {}): {} egna, {} från andra hållet, {} leverantörer", "  ".repeat(d), name, d, forward, reverse, suppliers.len());
            let mut followed = 0;
            for r in &suppliers {
                let shown = followed < per_level;
                if let Some(c) = r.relation.counterparty.cik {
                    if !visited.contains(&c) && shown {
                        visited.insert(c);
                        next.push(c);
                        followed += 1;
                    }
                }
                if shown {
                    print!("{}", "  ".repeat(d + 1));
                    print_relation(r);
                }
            }
        }
        level = next;
        if level.is_empty() {
            break;
        }
    }
    println!("{} bolag besökta. Allt väntar på granskning: term review list", visited.len());
    Ok(())
}

fn resolve_target(store: &Store, target: &str) -> Result<Cik> {
    if let Ok(cik) = target.parse::<Cik>() {
        return Ok(cik);
    }
    match store.company_by_ticker(target)? {
        Some(company) => Ok(company.cik),
        None => bail!("{target:?} är varken ett CIK eller en ticker i bolagslistan"),
    }
}

fn status_label(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Pending => "väntar",
        ReviewStatus::Approved => "godkänd",
        ReviewStatus::Rejected => "avvisad",
    }
}

fn print_relation(r: &StoredRelation) {
    let amount = match &r.relation.amount {
        Some(a) if a.unit == Unit::Percent => format!("{} %", a.value),
        Some(a) => format!("{:.0} USD", a.value),
        None => "utan belopp".to_string(),
    };
    let cik = match r.relation.counterparty.cik {
        Some(c) => format!("CIK {c}"),
        None => "ej matchad".to_string(),
    };
    println!(
        "  #{:<4} {:<8} {:<10} {:<12} {} ({}){}",
        r.id,
        status_label(r.status),
        format!("{:?}", r.relation.confidence),
        amount,
        r.relation.counterparty.name,
        cik,
        if r.relation.corroborated { " [bekräftad av båda]" } else { "" }
    );
    println!("        \"{}\"", r.relation.evidence.quote);
    if let Some(note) = &r.note {
        println!("        not: {note}");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Keys live in a local .env file that is never committed. A missing file is fine.
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let open = || Store::open(&cli.db).with_context(|| format!("kunde inte öppna {}", cli.db.display()));

    match cli.command {
        Command::Passages { file, min_score } => {
            let text = read_as_text(&file)?;
            let passages = select_passages(&text, min_score);
            for p in &passages {
                println!("[stycke {} | poäng {} | {}]\n{}\n", p.index, p.score, p.hits.join(", "), p.text);
            }
            eprintln!("{} av {} stycken valda", passages.len(), text.lines().count());
        }

        Command::Chain { file, cik, extractor, min_score, facit } => {
            let text = read_as_text(&file)?;
            let out = run_chain(extractor, cik, &text, &SourceRef::default(), min_score).await?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            eprintln!(
                "{}: {} stycken, {} godkända av citatkontrollen, {} avvisade",
                out.extractor,
                out.passages.len(),
                out.accepted.len(),
                out.rejected.len()
            );
            if let Some(path) = facit {
                let raw = std::fs::read_to_string(&path)
                    .with_context(|| format!("kunde inte läsa {}", path.display()))?;
                let rows: Vec<FacitRow> = serde_json::from_str(&raw).context("facit är inte giltig JSON")?;
                let s = evaluate(&out.accepted, &rows);
                eprintln!(
                    "precision {:.2} ({} av {}), täckning {:.2} ({} av {})",
                    s.precision, s.correct, s.predicted, s.recall, s.correct, s.expected
                );
                for m in &s.missed {
                    eprintln!("  missad: {m}");
                }
                for w in &s.wrong {
                    eprintln!("  fel: {w}");
                }
            }
        }

        Command::Ingest { file, cik, name, ticker, extractor, min_score, accession, url } => {
            let store = open()?;
            if let Some(name) = name {
                store.upsert_company(&Company { cik, name, ticker })?;
            }
            let text = read_as_text(&file)?;
            let source = SourceRef { accession, url };
            let out = run_chain(extractor, cik, &text, &source, min_score).await?;

            // Step 5: match names against the directory.
            let directory = Directory::new(store.companies()?);
            let mut relations = out.accepted;
            for relation in &mut relations {
                match directory.apply(relation) {
                    Resolution::Fuzzy { cik: guess, score } => eprintln!(
                        "  osäker matchning: {:?} liknar CIK {guess} ({score:.2}). Inte tillämpad",
                        relation.counterparty.name
                    ),
                    Resolution::Ambiguous(ciks) => eprintln!(
                        "  tvetydig: {:?} passar {} bolag. Inte tillämpad",
                        relation.counterparty.name,
                        ciks.len()
                    ),
                    Resolution::Exact(_) | Resolution::Unknown => {}
                }
            }

            // Step 7, then into the review queue (step 8).
            let (mut inserted, mut updated, mut unchanged) = (0, 0, 0);
            for relation in merge(relations) {
                match store.upsert_relation(&relation)? {
                    Upsert::Inserted(_) => inserted += 1,
                    Upsert::Updated(_) => updated += 1,
                    Upsert::Unchanged(_) => unchanged += 1,
                }
            }
            println!(
                "{}: {} stycken lästa, {} citat avvisade. {} nya relationer, {} ändrade, {} oförändrade.",
                out.extractor,
                out.passages.len(),
                out.rejected.len(),
                inserted,
                updated,
                unchanged
            );
            println!("Nya och ändrade väntar på granskning: term review list");
        }

        Command::Fetch { cik, out } => {
            let client = edgar_client()?;
            let filings = client.filings(cik).await?;
            let Some(filing) = latest_of_form(&filings, "10-K") else {
                bail!("hittade ingen 10-K för CIK {cik}");
            };
            let url = document_url(cik, filing);
            let body = client.get_text(&url).await?;
            std::fs::write(&out, &body).with_context(|| format!("kunde inte skriva {}", out.display()))?;
            println!("{} {} från {}", filing.form, filing.accession, filing.filing_date);
            println!("{url}");
            println!("sparad i {}", out.display());
            println!(
                "nästa steg: term ingest {} --cik {} --accession {} --url {}",
                out.display(),
                cik.unpadded(),
                filing.accession,
                url
            );
        }

        Command::Reverse { target, ingest, extractor } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let client = edgar_client()?;
            let found = reverse_pass(&store, &client, cik, ingest, true, extractor).await?;
            if ingest > 0 {
                println!("{found} relationer funna från andra hållet. De väntar på granskning: term review list");
            }
        }

        Command::Crawl { target, depth, per_level, reverse_cap, extractor } => {
            let store = open()?;
            let root = resolve_target(&store, &target)?;
            let client = edgar_client()?;
            crawl(&store, &client, root, depth, per_level, reverse_cap, extractor).await?;
        }

        Command::Named { target, file, url } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let directory = Directory::new(store.companies()?);
            let raw = std::fs::read_to_string(&file).with_context(|| format!("kunde inte läsa {}", file.display()))?;
            // A named relation must never replace stronger evidence for the same company.
            let stronger: std::collections::HashSet<term_core::Cik> = store
                .chain(cik, None)?
                .suppliers
                .iter()
                .filter(|r| r.relation.confidence != term_core::Confidence::Named)
                .filter_map(|r| r.relation.counterparty.cik)
                .collect();
            let (mut total, mut matched, mut kept) = (0, 0, 0);
            for line in raw.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                let mut relation = term_core::Relation {
                    subject: cik,
                    counterparty: term_core::Counterparty { name: line.to_string(), cik: None },
                    direction: Direction::Supplier,
                    amount: None,
                    period: None,
                    evidence: term_core::Evidence { accession: None, url: Some(url.clone()), quote: line.to_string() },
                    confidence: term_core::Confidence::Named,
                    corroborated: false,
                };
                directory.apply(&mut relation);
                if relation.counterparty.cik.is_some() {
                    matched += 1;
                }
                if relation.counterparty.cik.is_some_and(|c| stronger.contains(&c)) {
                    kept += 1;
                    continue;
                }
                store.upsert_relation(&relation)?;
                total += 1;
            }
            println!("{total} namngivna leverantörer inlästa, {matched} kopplade till ett bolag i SEC-listan, {kept} hoppade över för att starkare belägg redan finns. De väntar på granskning.");
        }

        Command::Companies { action } => {
            let store = open()?;
            match action {
                CompaniesAction::Import { file } => {
                    let raw = std::fs::read_to_string(&file)
                        .with_context(|| format!("kunde inte läsa {}", file.display()))?;
                    let directory = Directory::from_sec_tickers_json(&raw)?;
                    let n = store.upsert_companies(directory.companies())?;
                    println!("{n} bolag inlästa");
                }
                CompaniesAction::Add { cik, name, ticker } => {
                    store.upsert_company(&Company { cik, name, ticker })?;
                    println!("sparat");
                }
                CompaniesAction::List => {
                    for c in store.companies()? {
                        println!("{}  {:<8} {}", c.cik, c.ticker.unwrap_or_default(), c.name);
                    }
                }
            }
        }

        Command::Show { target, all } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let filter = if all { None } else { Some(ReviewStatus::Approved) };
            let view = store.chain(cik, filter)?;
            let name = view.company.map(|c| c.name).unwrap_or_else(|| "okänt bolag".into());
            println!("{name} (CIK {cik})");
            for (title, list) in [("LEVERANTÖRER", &view.suppliers), ("KUNDER", &view.customers)] {
                println!("\n{title}");
                if list.is_empty() {
                    println!("  inga");
                }
                for r in list {
                    print_relation(r);
                }
            }
            if !all {
                let waiting = store
                    .relations_for(cik, Some(ReviewStatus::Pending))?
                    .iter()
                    .filter(|r| matches!(r.relation.direction, Direction::Customer | Direction::Supplier))
                    .count();
                if waiting > 0 {
                    println!("\n{waiting} relationer väntar på granskning och visas inte. Lägg till --all för att se dem.");
                }
            }
        }

        Command::Review { action } => {
            let store = open()?;
            match action {
                ReviewAction::List => {
                    let queue = store.relations_with_status(ReviewStatus::Pending)?;
                    if queue.is_empty() {
                        println!("Granskningskön är tom.");
                    }
                    for r in &queue {
                        let role = match r.relation.direction {
                            Direction::Customer => "kund till",
                            Direction::Supplier => "leverantör till",
                        };
                        println!("{} CIK {}:", role, r.relation.subject);
                        print_relation(r);
                    }
                }
                ReviewAction::Approve { id, note } => {
                    let r = store.set_review(id, ReviewStatus::Approved, note.as_deref())?;
                    println!("#{} {} godkänd", r.id, r.relation.counterparty.name);
                }
                ReviewAction::Reject { id, note } => {
                    let r = store.set_review(id, ReviewStatus::Rejected, note.as_deref())?;
                    println!("#{} {} avvisad", r.id, r.relation.counterparty.name);
                }
            }
        }

        Command::Serve { addr } => {
            let store = Arc::new(open()?);
            if !addr.ip().is_loopback() {
                eprintln!("VARNING: servern har ingen inloggning och lyssnar nu utanför den här datorn ({addr}).");
            }
            println!("lyssnar på http://{addr}  (prova /health)");
            term_api::serve(addr, store).await?;
        }

        Command::Events { action } => {
            let store = open()?;
            match action {
                EventsAction::Fetch { source, feed } => {
                    let http = Http::new()?;
                    if source != EventSource::Gdacs {
                        let events = usgs::fetch(&http, &feed).await?;
                        let new = store.upsert_events(&events)?;
                        println!("usgs ({feed}): {} händelser hämtade, {new} nya", events.len());
                    }
                    if source != EventSource::Usgs {
                        let events = gdacs::fetch(&http).await?;
                        let new = store.upsert_events(&events)?;
                        println!("gdacs (orange och röd): {} händelser hämtade, {new} nya", events.len());
                    }
                }
                EventsAction::List { limit } => {
                    let events = store.events(limit)?;
                    if events.is_empty() {
                        println!("Inga händelser sparade. Hämta med: term events fetch");
                    }
                    for e in events {
                        let position = match (e.lat, e.lon) {
                            (Some(lat), Some(lon)) => format!("{lat:.1}, {lon:.1}"),
                            _ => "okänd plats".to_string(),
                        };
                        println!(
                            "{}  {:<6} {:<10} {:<7} {}  ({position})",
                            unix_to_iso_minutes(e.time),
                            e.source,
                            e.kind,
                            e.alert.unwrap_or_default(),
                            e.title
                        );
                    }
                }
            }
        }

        Command::Series { action } => {
            let store = open()?;
            match action {
                SeriesAction::Fetch { source, id, last, from, to, country } => {
                    let http = Http::new()?;
                    let today = unix_to_iso_minutes(now_unix())[..10].to_string();
                    let month_ago = unix_to_iso_minutes(now_unix() - 30 * 86_400)[..10].to_string();
                    let (name, series, points) = match source {
                        SeriesSource::Ecb => ("ecb", ecb::series_id(&id), ecb::fetch(&http, &id, last).await?),
                        SeriesSource::Riksbank => {
                            let from = from.unwrap_or(month_ago);
                            let to = to.unwrap_or(today);
                            ("riksbank", id.trim().to_string(), riksbank::fetch(&http, &id, &from, &to).await?)
                        }
                        SeriesSource::Worldbank => (
                            "worldbank",
                            worldbank::series_id(&country, &id),
                            worldbank::fetch(&http, &country, &id, 100).await?,
                        ),
                        SeriesSource::Fred => ("fred", id.trim().to_string(), fred::fetch(&http, &id).await?),
                    };
                    let changed = store.upsert_series(name, &series, &points)?;
                    println!("{name} {series}: {} observationer hämtade, {changed} nya eller ändrade", points.len());
                    if let Some(p) = points.last() {
                        println!("senaste: {} = {}", p.date, p.value);
                    }
                }
                SeriesAction::Show { source, series, last } => {
                    let points = store.series(&source, &series)?;
                    if points.is_empty() {
                        println!("Serien finns inte sparad. Se: term series list");
                    }
                    for p in points.iter().skip(points.len().saturating_sub(last)) {
                        println!("{}  {}", p.date, p.value);
                    }
                }
                SeriesAction::List => {
                    for (source, series, n, last) in store.series_list()? {
                        println!("{source:<10} {series:<28} {n:>6} punkter, senaste {last}");
                    }
                }
            }
        }

        Command::Lookup { action } => {
            let http = Http::new()?;
            match action {
                LookupAction::Lei { name } => {
                    let records = gleif::search(&http, &name, 10).await?;
                    if records.is_empty() {
                        println!("Ingen träff hos GLEIF.");
                    }
                    for r in records {
                        println!(
                            "{}  {}  ({}, {})",
                            r.lei,
                            r.legal_name,
                            r.jurisdiction.unwrap_or_else(|| "okänd jurisdiktion".into()),
                            r.entity_status.unwrap_or_else(|| "okänd status".into())
                        );
                    }
                }
                LookupAction::Figi { ticker, exch } => {
                    let results = openfigi::map_tickers(&http, &[(ticker.as_str(), exch.as_str())]).await?;
                    let records = results.into_iter().next().unwrap_or_default();
                    if records.is_empty() {
                        println!("Ingen träff hos OpenFIGI för {ticker} på {exch}.");
                    }
                    for r in records {
                        println!(
                            "{}  {}  {}  ({})",
                            r.figi,
                            r.ticker.unwrap_or_default(),
                            r.name.unwrap_or_default(),
                            r.security_type.unwrap_or_default()
                        );
                    }
                }
            }
        }

        Command::Sanctions { action } => {
            let store = open()?;
            let caveat = "Obs: bara huvudnamn jämförs, inte alias. Ingen träff betyder inte att parten är fri från sanktioner.";
            match action {
                SanctionsAction::Fetch => {
                    let entries = ofac::fetch(&Http::new()?).await?;
                    let n = store.replace_sanctions("ofac", &entries)?;
                    println!("{n} namn sparade från OFAC:s SDN-lista.");
                }
                SanctionsAction::Check { name } => {
                    if store.sanction_count()? == 0 {
                        bail!("listan är inte hämtad. Kör först: term sanctions fetch");
                    }
                    let hits = store.sanction_matches(&name)?;
                    if hits.is_empty() {
                        println!("Ingen träff på huvudnamn för {name:?}.");
                    }
                    for h in hits {
                        println!("TRÄFF  #{}  {}  program: {}", h.list_id, h.name, h.program.unwrap_or_else(|| "okänt".into()));
                    }
                    println!("{caveat}");
                }
                SanctionsAction::Screen => {
                    if store.sanction_count()? == 0 {
                        bail!("listan är inte hämtad. Kör först: term sanctions fetch");
                    }
                    let relations = store.all_relations()?;
                    let mut hits = 0;
                    for r in &relations {
                        for h in store.sanction_matches(&r.relation.counterparty.name)? {
                            hits += 1;
                            println!(
                                "TRÄFF  relation #{}  {}  motsvarar listans #{} {}",
                                r.id, r.relation.counterparty.name, h.list_id, h.name
                            );
                        }
                    }
                    println!("{} motparter jämförda, {hits} träffar.", relations.len());
                    println!("{caveat}");
                }
            }
        }

        Command::Fin { target } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let name = store.company(cik)?.map(|c| c.name).unwrap_or_else(|| format!("CIK {cik}"));
            let json = edgar_client()?.company_facts(cik).await?;
            let lines = facts::statement(&json)?;
            if lines.is_empty() {
                bail!("SEC har inga årsvärden i US-dollar för {name}");
            }
            println!("{name}, årsvärden i miljarder USD ur bolagets egna 10-K (SEC company facts)");
            for (label, tag, points) in &lines {
                store.upsert_series("sec-fin", &format!("{} {label}", cik.unpadded()), points)?;
                let recent: Vec<String> = points
                    .iter()
                    .skip(points.len().saturating_sub(4))
                    .map(|p| format!("{} {:>8.1}", &p.date[..4], p.value / 1e9))
                    .collect();
                println!("  {label:<14} {}   (tagg {tag})", recent.join("   "));
            }
            println!("Sparat som serier. Se dem i vyn SERIER eller med: term series list");
        }

        Command::Insiders { target } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let filings = edgar_client()?.filings(cik).await?;
            let mut forms: Vec<_> = filings.iter().filter(|f| f.form == "4").collect();
            forms.sort_by(|a, b| b.filing_date.cmp(&a.filing_date));
            let since = unix_to_iso_minutes(now_unix() - 90 * 86_400)[..10].to_string();
            let recent = forms.iter().filter(|f| f.filing_date >= since).count();
            println!("{} insideranmälningar bland de senaste inlämningarna, varav {recent} de senaste 90 dagarna.", forms.len());
            for f in forms.iter().take(10) {
                println!("  {}  {}", f.filing_date, document_url(cik, f));
            }
            println!("Anmälningarna listas bara. Köp, sälj och belopp läses inte ut än.");
        }

        Command::World { action } => {
            let store = open()?;
            match action {
                WorldAction::Build { limit } => {
                    let http = Http::new()?;
                    let mut countries = world::fetch_countries(&http).await?;
                    countries.sort_by(|a, b| b.gdp_usd.unwrap_or(0.0).total_cmp(&a.gdp_usd.unwrap_or(0.0)));
                    println!("{} länder och territorier från Världsbanken.", countries.len());
                    let asked = limit.unwrap_or(countries.len());
                    let (mut ok, mut failed) = (0, 0);
                    for (i, country) in countries.iter_mut().enumerate() {
                        if i < asked {
                            // GLEIF allows about one request a second.
                            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                            match world::lei_count(&http, &country.iso2).await {
                                Ok(n) => {
                                    country.lei_count = Some(n);
                                    ok += 1;
                                }
                                Err(_) => failed += 1,
                            }
                            country.esef_count = world::esef_count(&http, &country.iso2).await.ok();
                        }
                        store.upsert_country(country)?;
                    }
                    println!("{ok} länder besvarade av GLEIF, {failed} misslyckades. Kör igen för att fylla luckor.");
                }
                WorldAction::List { limit } => {
                    println!("{:<4} {:<30} {:>12} {:>10} {:>7}  nationellt system", "kod", "land", "BNP mdr USD", "LEI", "ESEF");
                    for c in store.countries_of_world()?.into_iter().take(limit) {
                        println!(
                            "{:<4} {:<30} {:>12} {:>10} {:>7}  {}",
                            c.iso2,
                            c.name.chars().take(30).collect::<String>(),
                            c.gdp_usd.map(|g| format!("{:.1}", g / 1e9)).unwrap_or_else(|| "saknas".into()),
                            c.lei_count.map(|n| n.to_string()).unwrap_or_else(|| "ej frågat".into()),
                            c.esef_count.map(|n| n.to_string()).unwrap_or_else(|| "ej frågat".into()),
                            world::national_system(&c.iso2).map(|(n, _)| n).unwrap_or("")
                        );
                    }
                }
                WorldAction::Export { file } => {
                    let countries = store.countries_of_world()?;
                    let with_lei = countries.iter().filter(|c| c.lei_count.unwrap_or(0) > 0).count();
                    let with_esef = countries.iter().filter(|c| c.esef_count.unwrap_or(0) > 0).count();
                    let with_gdp = countries.iter().filter(|c| c.gdp_usd.is_some()).count();
                    let mut md = String::new();
                    md.push_str("# Världen: vad källorna har för varje land\n\n");
                    md.push_str(&format!(
                        "Skapad {} med `term world build`. Siffrorna är räknade av källorna själva, inte skrivna för hand.\n\n",
                        &unix_to_iso_minutes(now_unix())[..10]
                    ));
                    md.push_str(&format!(
                        "- **{}** länder och territorier i Världsbankens lista.\n- **{with_gdp}** har en BNP-siffra (Världsbanken).\n- **{with_lei}** har minst ett bolag med LEI (GLEIF).\n- **{with_esef}** har årsredovisningar i det europeiska formatet (filings.xbrl.org).\n\n",
                        countries.len()
                    ));
                    md.push_str("Gäller alla länder: jordbävningar och larm (USGS, GDACS) täcker hela världen, och OFAC:s sanktionslista gäller parter i alla länder.\n\n");
                    md.push_str("Kolumnen Nationellt system är bara ifylld där projektet har undersökt systemet. Tom ruta betyder inte att landet saknar ett, bara att ingen har kartlagt det.\n\n");
                    md.push_str("| # | Land | Kod | Region | BNP mdr USD | År | Bolag med LEI | ESEF | Nationellt system |\n|---|---|---|---|---|---|---|---|---|\n");
                    for (i, c) in countries.iter().enumerate() {
                        let national = match world::national_system(&c.iso2) {
                            Some((name, status)) => format!("{name} ({status})"),
                            None if c.esef_count.unwrap_or(0) > 0 => "filings.xbrl.org (inlagd och körd)".to_string(),
                            None => String::new(),
                        };
                        md.push_str(&format!(
                            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                            i + 1,
                            c.name,
                            c.iso2,
                            c.region,
                            c.gdp_usd.map(|g| format!("{:.1}", g / 1e9)).unwrap_or_default(),
                            c.gdp_year.map(|y| y.to_string()).unwrap_or_default(),
                            c.lei_count.map(|n| n.to_string()).unwrap_or_else(|| "ej frågat".into()),
                            c.esef_count.map(|n| n.to_string()).unwrap_or_else(|| "ej frågat".into()),
                            national
                        ));
                    }
                    std::fs::write(&file, md).with_context(|| format!("kunde inte skriva {}", file.display()))?;
                    println!("{} länder skrivna till {}", countries.len(), file.display());
                }
            }
        }

        Command::Analyze { target } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let a = analyze_company(&store, cik)?;
            println!("{} (CIK {cik})", a.company);
            println!(
                "{} leverantörer, {} kunder, {} ogranskade, {} kända platser, {} händelser senaste 30 dagarna",
                a.suppliers, a.customers, a.pending, a.places, a.events_in_window
            );
            if let Some(share) = a.largest_customer_share {
                println!("Största kund {share} %, namngivna kunder tillsammans {} %", a.disclosed_customer_share);
            }
            println!();
            for f in &a.findings {
                let mark = match f.level {
                    Level::Alert => "LARM ",
                    Level::Watch => "BEVAKA",
                    Level::Info => "INFO ",
                };
                println!("{mark:<7} {:<14} {}", f.area, f.text);
                println!("                       underlag: {}", f.basis);
            }
            println!();
            println!("Analysen beskriver exponering. Den säger inte vad någon bör köpa eller sälja.");
        }

        Command::Locations { action } => {
            let store = open()?;
            // An owner is a company in the directory, or any counterparty name.
            let owner_name = |raw: &str| -> Result<String> {
                Ok(match resolve_target(&store, raw) {
                    Ok(cik) => store.company(cik)?.map(|c| c.name).unwrap_or_else(|| raw.to_string()),
                    Err(_) => raw.to_string(),
                })
            };
            match action {
                LocationsAction::FetchEpa { owner, search, state } => {
                    let owner = owner_name(&owner)?;
                    let search = search.unwrap_or_else(|| normalize_name(&owner));
                    let places = epa_frs::fetch(&Http::new()?, &search, state.as_deref(), &owner).await?;
                    let n = store.upsert_locations(&places)?;
                    println!("{n} anläggningar med koordinater sparade för {owner} (sökord {search:?}).");
                    println!("Sökningen går på anläggningens namn och kan ge fel träffar. Gå igenom listan: term locations list");
                }
                LocationsAction::Add { owner, lat, lon, label } => {
                    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                        bail!("latitud ska ligga mellan -90 och 90, longitud mellan -180 och 180");
                    }
                    let owner = owner_name(&owner)?;
                    let external_id = format!("{}:{}", normalize_name(&owner), normalize_name(&label));
                    store.upsert_locations(&[Location { owner: owner.clone(), label, lat, lon, source: "manual".into(), external_id }])?;
                    println!("sparad för {owner}");
                }
                LocationsAction::List { owner } => {
                    let owner = owner.map(|o| owner_name(&o)).transpose()?;
                    let places = store.locations(owner.as_deref())?;
                    if places.is_empty() {
                        println!("Inga platser sparade.");
                    }
                    for p in places {
                        println!("{:<34} {:>9.4} {:>10.4}  {:<8} {}", p.owner, p.lat, p.lon, p.source, p.label);
                    }
                }
            }
        }

        Command::Subawards { target, from } => {
            let store = open()?;
            let cik = resolve_target(&store, &target)?;
            let Some(company) = store.company(cik)? else {
                bail!("CIK {cik} finns inte i bolagslistan. Lägg till det med: term companies add");
            };
            let to = unix_to_iso_minutes(now_unix())[..10].to_string();
            let from = from.unwrap_or_else(|| unix_to_iso_minutes(now_unix() - 365 * 86_400)[..10].to_string());
            let search = normalize_name(&company.name);
            let rows = usaspending::fetch(&Http::new()?, &search, &from, &to, 100).await?;
            let relations = usaspending::customer_relations(cik, &company.name, &rows);
            let (mut new, mut changed) = (0, 0);
            for relation in &relations {
                match store.upsert_relation(relation)? {
                    Upsert::Inserted(_) => new += 1,
                    Upsert::Updated(_) => changed += 1,
                    Upsert::Unchanged(_) => {}
                }
            }
            println!(
                "{} underkontrakt hämtade för {} ({from} till {to}). {} kunder funna: {new} nya, {changed} ändrade.",
                rows.len(),
                company.name,
                relations.len()
            );
            if relations.is_empty() && !rows.is_empty() {
                println!("Raderna gällde bolag med liknande namn. Prova bolagets exakta registrerade namn.");
            }
            println!("De väntar på granskning: term review list");
        }

        Command::Esef { country, limit } => {
            let filings = esef::latest(&Http::new()?, &country, limit).await?;
            if filings.is_empty() {
                println!("Inga årsredovisningar för {country}.");
            }
            for f in filings {
                println!(
                    "{}  {}  period till {}  {}",
                    f.lei,
                    f.entity_name.unwrap_or_else(|| "okänt namn".into()),
                    f.period_end,
                    f.report_url.unwrap_or_default()
                );
            }
        }

        Command::Signal { action } => {
            let store = open()?;
            match action {
                SignalAction::VerifyLog => {
                    let records = store.log()?;
                    term_signal::log::verify(&records)?;
                    match records.last() {
                        Some(last) => println!("{} rader, kedjan är hel. Senaste kontrollsumma: {}", records.len(), last.hash),
                        None => println!("Signalloggen är tom."),
                    }
                }
            }
        }
    }
    Ok(())
}

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
use term_sources::{ecb, esef, fred, gdacs, gleif, ofac, openfigi, riksbank, usgs, worldbank, Http};
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
    Reverse { cik: Cik },
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
    /// Senaste europeiska årsredovisningar (ESEF) för ett land, till exempel SE
    Esef {
        country: String,
        #[arg(long, default_value_t = 10)]
        limit: u32,
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
    })
}

fn edgar_client() -> Result<EdgarClient> {
    let Ok(user_agent) = std::env::var("TERM_USER_AGENT") else {
        bail!("sätt TERM_USER_AGENT till vem ni är och en kontaktadress, till exempel \"Bolaget AB anna@bolaget.se\"");
    };
    Ok(EdgarClient::new(&user_agent)?)
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
        Some(a) => format!("{} USD", a.value),
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

        Command::Reverse { cik } => {
            let store = open()?;
            let Some(company) = store.company(cik)? else {
                bail!("CIK {cik} finns inte i bolagslistan. Lägg till det med: term companies add");
            };
            let client = edgar_client()?;
            let hits = client.full_text_search(&company.name, &["10-K"]).await?;
            let others = mentioning_companies(&hits, cik);
            println!("{} träffar. {} andra bolag nämner {:?}:", hits.len(), others.len(), company.name);
            for other in others {
                let name = store.company(other)?.map(|c| c.name).unwrap_or_else(|| "okänt bolag".into());
                println!("  CIK {other}  {name}");
            }
            println!("Hämta och läs in deras dokument för att få relationen bekräftad från andra hållet.");
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

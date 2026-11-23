//! Storage for companies, relations, review decisions and the signal log.
//!
//! DEVIATION FROM THE PLAN: the plan names PostgreSQL through `sqlx`. This
//! crate uses SQLite in a single file so that the prototype runs with no
//! server and its tests run anywhere. The schema is plain SQL and carries
//! over. Moving to PostgreSQL is a decision for phase 0.

use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use term_core::names::normalize_name;
use term_core::{
    Amount, Cik, Company, Confidence, Counterparty, Direction, Event, Evidence, Location, LogRecord, Period,
    Relation, ReviewStatus, SanctionEntry, SeriesPoint, StoredRelation, Unit,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("corrupt row: {0}")]
    Corrupt(String),
    #[error("relation {0} does not exist")]
    NoSuchRelation(i64),
    #[error("log record {got} does not follow record {expected_after}")]
    LogOutOfOrder { got: u64, expected_after: u64 },
}

pub type Result<T> = std::result::Result<T, StoreError>;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS company (
    cik    INTEGER PRIMARY KEY,
    name   TEXT NOT NULL,
    ticker TEXT
);
CREATE INDEX IF NOT EXISTS company_ticker ON company(ticker);

CREATE TABLE IF NOT EXISTS relation (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_cik       INTEGER NOT NULL,
    counterparty_name TEXT NOT NULL,
    counterparty_key  TEXT NOT NULL,
    counterparty_cik  INTEGER,
    direction         TEXT NOT NULL,
    amount_value      REAL,
    amount_unit       TEXT,
    amount_basis      TEXT,
    fiscal_year       INTEGER,
    accession         TEXT,
    url               TEXT,
    quote             TEXT NOT NULL,
    confidence        TEXT NOT NULL,
    corroborated      INTEGER NOT NULL DEFAULT 0,
    status            TEXT NOT NULL DEFAULT 'pending',
    note              TEXT,
    known_at          INTEGER NOT NULL,
    reviewed_at       INTEGER,
    UNIQUE (subject_cik, counterparty_key, direction)
);
CREATE INDEX IF NOT EXISTS relation_counterparty ON relation(counterparty_cik);

CREATE TABLE IF NOT EXISTS event (
    source        TEXT NOT NULL,
    external_id   TEXT NOT NULL,
    kind          TEXT NOT NULL,
    title         TEXT NOT NULL,
    time          INTEGER NOT NULL,
    lat           REAL,
    lon           REAL,
    severity      REAL,
    severity_unit TEXT,
    alert         TEXT,
    country       TEXT,
    url           TEXT,
    known_at      INTEGER NOT NULL,
    PRIMARY KEY (source, external_id)
);
CREATE INDEX IF NOT EXISTS event_time ON event(time);

CREATE TABLE IF NOT EXISTS series_point (
    source   TEXT NOT NULL,
    series   TEXT NOT NULL,
    date     TEXT NOT NULL,
    value    REAL NOT NULL,
    known_at INTEGER NOT NULL,
    PRIMARY KEY (source, series, date)
);

CREATE TABLE IF NOT EXISTS sanction (
    source   TEXT NOT NULL,
    list_id  INTEGER NOT NULL,
    name     TEXT NOT NULL,
    name_key TEXT NOT NULL,
    kind     TEXT,
    program  TEXT,
    PRIMARY KEY (source, list_id)
);
CREATE INDEX IF NOT EXISTS sanction_name ON sanction(name_key);

CREATE TABLE IF NOT EXISTS location (
    source      TEXT NOT NULL,
    external_id TEXT NOT NULL,
    owner       TEXT NOT NULL,
    owner_key   TEXT NOT NULL,
    label       TEXT NOT NULL,
    lat         REAL NOT NULL,
    lon         REAL NOT NULL,
    PRIMARY KEY (source, external_id)
);
CREATE INDEX IF NOT EXISTS location_owner ON location(owner_key);

CREATE TABLE IF NOT EXISTS signal_log (
    seq        INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    payload    TEXT NOT NULL,
    prev_hash  TEXT NOT NULL,
    hash       TEXT NOT NULL
);
";

const RELATION_COLUMNS: &str = "id, subject_cik, counterparty_name, counterparty_cik, direction, \
    amount_value, amount_unit, amount_basis, fiscal_year, accession, url, quote, confidence, \
    corroborated, status, note, known_at, reviewed_at";

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// What [`Store::upsert_relation`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Inserted(i64),
    /// Same content as before. Review status is untouched.
    Unchanged(i64),
    /// The content changed, so the relation went back to pending.
    Updated(i64),
}

impl Upsert {
    pub fn id(self) -> i64 {
        match self {
            Upsert::Inserted(id) | Upsert::Unchanged(id) | Upsert::Updated(id) => id,
        }
    }
}

/// Everything known about one company's neighbours.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainView {
    pub company: Option<Company>,
    pub suppliers: Vec<StoredRelation>,
    pub customers: Vec<StoredRelation>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid query.
        // SQLite itself is still consistent, so keep going.
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ----- companies -----

    pub fn upsert_company(&self, company: &Company) -> Result<()> {
        self.conn().execute(
            "INSERT INTO company (cik, name, ticker) VALUES (?1, ?2, ?3)
             ON CONFLICT(cik) DO UPDATE SET name = excluded.name, ticker = excluded.ticker",
            params![company.cik.get() as i64, company.name, company.ticker],
        )?;
        Ok(())
    }

    pub fn upsert_companies(&self, companies: &[Company]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO company (cik, name, ticker) VALUES (?1, ?2, ?3)
                 ON CONFLICT(cik) DO UPDATE SET name = excluded.name, ticker = excluded.ticker",
            )?;
            for c in companies {
                stmt.execute(params![c.cik.get() as i64, c.name, c.ticker])?;
            }
        }
        tx.commit()?;
        Ok(companies.len())
    }

    pub fn company(&self, cik: Cik) -> Result<Option<Company>> {
        self.conn()
            .query_row(
                "SELECT cik, name, ticker FROM company WHERE cik = ?1",
                params![cik.get() as i64],
                company_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn company_by_ticker(&self, ticker: &str) -> Result<Option<Company>> {
        self.conn()
            .query_row(
                "SELECT cik, name, ticker FROM company WHERE upper(ticker) = upper(?1) ORDER BY cik LIMIT 1",
                params![ticker.trim()],
                company_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn companies(&self) -> Result<Vec<Company>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT cik, name, ticker FROM company ORDER BY cik")?;
        let rows = stmt.query_map([], company_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    // ----- relations -----

    /// Insert a relation, or update the one that already links the same
    /// subject, counterparty and direction.
    ///
    /// A relation whose content changes goes back to `pending`, because an
    /// approval applies to the evidence the reviewer saw.
    pub fn upsert_relation(&self, relation: &Relation) -> Result<Upsert> {
        self.upsert_relation_at(relation, now_unix())
    }

    pub fn upsert_relation_at(&self, relation: &Relation, now: i64) -> Result<Upsert> {
        let key = match relation.counterparty.cik {
            Some(cik) => format!("cik:{cik}"),
            None => format!("name:{}", normalize_name(&relation.counterparty.name)),
        };
        let direction = direction_str(relation.direction);
        let conn = self.conn();

        let existing: Option<StoredRelation> = conn
            .query_row(
                &format!(
                    "SELECT {RELATION_COLUMNS} FROM relation
                     WHERE subject_cik = ?1 AND counterparty_key = ?2 AND direction = ?3"
                ),
                params![relation.subject.get() as i64, key, direction],
                relation_from_row,
            )
            .optional()?
            .transpose()?;

        let (value, unit, basis) = match &relation.amount {
            Some(a) => (Some(a.value), Some(unit_str(a.unit)), a.basis.clone()),
            None => (None, None, None),
        };
        let year = relation.period.map(|p| p.fiscal_year as i64);
        let counterparty_cik = relation.counterparty.cik.map(|c| c.get() as i64);

        match existing {
            Some(old) if same_content(&old.relation, relation) => Ok(Upsert::Unchanged(old.id)),
            Some(old) => {
                conn.execute(
                    "UPDATE relation SET counterparty_name = ?1, counterparty_cik = ?2,
                        amount_value = ?3, amount_unit = ?4, amount_basis = ?5, fiscal_year = ?6,
                        accession = ?7, url = ?8, quote = ?9, confidence = ?10, corroborated = ?11,
                        status = 'pending', note = NULL, known_at = ?12, reviewed_at = NULL
                     WHERE id = ?13",
                    params![
                        relation.counterparty.name,
                        counterparty_cik,
                        value,
                        unit,
                        basis,
                        year,
                        relation.evidence.accession,
                        relation.evidence.url,
                        relation.evidence.quote,
                        confidence_str(relation.confidence),
                        relation.corroborated,
                        now,
                        old.id
                    ],
                )?;
                Ok(Upsert::Updated(old.id))
            }
            None => {
                conn.execute(
                    "INSERT INTO relation (subject_cik, counterparty_name, counterparty_key,
                        counterparty_cik, direction, amount_value, amount_unit, amount_basis,
                        fiscal_year, accession, url, quote, confidence, corroborated, known_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        relation.subject.get() as i64,
                        relation.counterparty.name,
                        key,
                        counterparty_cik,
                        direction,
                        value,
                        unit,
                        basis,
                        year,
                        relation.evidence.accession,
                        relation.evidence.url,
                        relation.evidence.quote,
                        confidence_str(relation.confidence),
                        relation.corroborated,
                        now
                    ],
                )?;
                Ok(Upsert::Inserted(conn.last_insert_rowid()))
            }
        }
    }

    pub fn relation(&self, id: i64) -> Result<Option<StoredRelation>> {
        self.conn()
            .query_row(
                &format!("SELECT {RELATION_COLUMNS} FROM relation WHERE id = ?1"),
                params![id],
                relation_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Relations where `cik` is the subject. `status = None` returns all.
    pub fn relations_for(&self, cik: Cik, status: Option<ReviewStatus>) -> Result<Vec<StoredRelation>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RELATION_COLUMNS} FROM relation
             WHERE subject_cik = ?1 AND (?2 IS NULL OR status = ?2)
             ORDER BY direction, amount_value IS NULL, amount_value DESC, counterparty_name"
        ))?;
        let rows = stmt.query_map(params![cik.get() as i64, status.map(status_str)], relation_from_row)?;
        collect(rows)
    }

    pub fn relations_with_status(&self, status: ReviewStatus) -> Result<Vec<StoredRelation>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RELATION_COLUMNS} FROM relation WHERE status = ?1 ORDER BY known_at, id"
        ))?;
        let rows = stmt.query_map(params![status_str(status)], relation_from_row)?;
        collect(rows)
    }

    pub fn all_relations(&self) -> Result<Vec<StoredRelation>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {RELATION_COLUMNS} FROM relation ORDER BY id"))?;
        let rows = stmt.query_map([], relation_from_row)?;
        collect(rows)
    }

    pub fn chain(&self, cik: Cik, status: Option<ReviewStatus>) -> Result<ChainView> {
        let company = self.company(cik)?;
        let (customers, suppliers) = self
            .relations_for(cik, status)?
            .into_iter()
            .partition(|r| r.relation.direction == Direction::Customer);
        Ok(ChainView { company, suppliers, customers })
    }

    /// Step 8: record a person's decision.
    pub fn set_review(&self, id: i64, status: ReviewStatus, note: Option<&str>) -> Result<StoredRelation> {
        let reviewed_at = match status {
            ReviewStatus::Pending => None,
            _ => Some(now_unix()),
        };
        let changed = self.conn().execute(
            "UPDATE relation SET status = ?1, note = ?2, reviewed_at = ?3 WHERE id = ?4",
            params![status_str(status), note, reviewed_at, id],
        )?;
        if changed == 0 {
            return Err(StoreError::NoSuchRelation(id));
        }
        self.relation(id)?.ok_or(StoreError::NoSuchRelation(id))
    }

    // ----- events -----

    /// Store events. An event that is already known keeps its first
    /// `known_at`, so that a historical test never sees it earlier than
    /// the store did. Returns how many were new.
    pub fn upsert_events(&self, events: &[Event]) -> Result<usize> {
        self.upsert_events_at(events, now_unix())
    }

    pub fn upsert_events_at(&self, events: &[Event], now: i64) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut new = 0;
        {
            let mut insert = tx.prepare(
                "INSERT INTO event (source, external_id, kind, title, time, lat, lon, severity,
                    severity_unit, alert, country, url, known_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(source, external_id) DO NOTHING",
            )?;
            let mut update = tx.prepare(
                "UPDATE event SET kind = ?3, title = ?4, time = ?5, lat = ?6, lon = ?7, severity = ?8,
                    severity_unit = ?9, alert = ?10, country = ?11, url = ?12
                 WHERE source = ?1 AND external_id = ?2",
            )?;
            for e in events {
                let inserted = insert.execute(params![
                    e.source, e.external_id, e.kind, e.title, e.time, e.lat, e.lon, e.severity,
                    e.severity_unit, e.alert, e.country, e.url, now
                ])?;
                if inserted == 1 {
                    new += 1;
                } else {
                    // Sources revise magnitude and alert level after the fact.
                    update.execute(params![
                        e.source, e.external_id, e.kind, e.title, e.time, e.lat, e.lon, e.severity,
                        e.severity_unit, e.alert, e.country, e.url
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(new)
    }

    pub fn event_count(&self) -> Result<u64> {
        let n: i64 = self.conn().query_row("SELECT count(*) FROM event", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Newest first.
    pub fn events(&self, limit: u32) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT source, external_id, kind, title, time, lat, lon, severity, severity_unit, alert,
                    country, url
             FROM event ORDER BY time DESC, source, external_id LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(Event {
                source: r.get(0)?,
                external_id: r.get(1)?,
                kind: r.get(2)?,
                title: r.get(3)?,
                time: r.get(4)?,
                lat: r.get(5)?,
                lon: r.get(6)?,
                severity: r.get(7)?,
                severity_unit: r.get(8)?,
                alert: r.get(9)?,
                country: r.get(10)?,
                url: r.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ----- time series -----

    /// Store observations. A value that the source has revised is updated
    /// and gets a new `known_at`. Returns how many points were new or changed.
    pub fn upsert_series(&self, source: &str, series: &str, points: &[SeriesPoint]) -> Result<usize> {
        self.upsert_series_at(source, series, points, now_unix())
    }

    pub fn upsert_series_at(&self, source: &str, series: &str, points: &[SeriesPoint], now: i64) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut changed = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO series_point (source, series, date, value, known_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source, series, date) DO UPDATE SET value = excluded.value, known_at = excluded.known_at
                 WHERE series_point.value <> excluded.value",
            )?;
            for p in points {
                changed += stmt.execute(params![source, series, p.date, p.value, now])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Oldest first.
    pub fn series(&self, source: &str, series: &str) -> Result<Vec<SeriesPoint>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT date, value FROM series_point WHERE source = ?1 AND series = ?2 ORDER BY date",
        )?;
        let rows = stmt.query_map(params![source, series], |r| Ok(SeriesPoint { date: r.get(0)?, value: r.get(1)? }))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every stored series as (source, series, number of points, last date).
    pub fn series_list(&self) -> Result<Vec<(String, String, u64, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT source, series, count(*), max(date) FROM series_point GROUP BY source, series ORDER BY source, series",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64, r.get(3)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ----- sanctions -----

    /// Replace everything stored from one list with a fresh copy.
    pub fn replace_sanctions(&self, source: &str, entries: &[SanctionEntry]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sanction WHERE source = ?1", params![source])?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO sanction (source, list_id, name, name_key, kind, program)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for e in entries {
                stmt.execute(params![source, e.list_id as i64, e.name, normalize_name(&e.name), e.kind, e.program])?;
            }
        }
        tx.commit()?;
        Ok(entries.len())
    }

    pub fn sanction_count(&self) -> Result<u64> {
        let n: i64 = self.conn().query_row("SELECT count(*) FROM sanction", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Entries whose normalised primary name equals the normalised query.
    /// Aliases are not stored, so an empty answer proves nothing.
    pub fn sanction_matches(&self, name: &str) -> Result<Vec<SanctionEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT list_id, name, kind, program FROM sanction WHERE name_key = ?1 ORDER BY list_id",
        )?;
        let rows = stmt.query_map(params![normalize_name(name)], |r| {
            Ok(SanctionEntry {
                list_id: r.get::<_, i64>(0)? as u64,
                name: r.get(1)?,
                kind: r.get(2)?,
                program: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ----- locations -----

    pub fn upsert_locations(&self, locations: &[Location]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO location (source, external_id, owner, owner_key, label, lat, lon)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(source, external_id) DO UPDATE SET owner = excluded.owner,
                    owner_key = excluded.owner_key, label = excluded.label, lat = excluded.lat, lon = excluded.lon",
            )?;
            for l in locations {
                stmt.execute(params![l.source, l.external_id, l.owner, normalize_name(&l.owner), l.label, l.lat, l.lon])?;
            }
        }
        tx.commit()?;
        Ok(locations.len())
    }

    /// Places that belong to the named company. `None` returns all.
    pub fn locations(&self, owner: Option<&str>) -> Result<Vec<Location>> {
        let conn = self.conn();
        let key = owner.map(normalize_name);
        let mut stmt = conn.prepare(
            "SELECT owner, label, lat, lon, source, external_id FROM location
             WHERE ?1 IS NULL OR owner_key = ?1 ORDER BY owner, label, external_id",
        )?;
        let rows = stmt.query_map(params![key], |r| {
            Ok(Location {
                owner: r.get(0)?,
                label: r.get(1)?,
                lat: r.get(2)?,
                lon: r.get(3)?,
                source: r.get(4)?,
                external_id: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ----- signal log -----

    /// Append one record. The record must follow the last one stored.
    /// Hashes are checked by `term-signal`, not here.
    pub fn append_log(&self, record: &LogRecord) -> Result<()> {
        let conn = self.conn();
        let last: Option<i64> = conn.query_row("SELECT max(seq) FROM signal_log", [], |r| r.get(0))?;
        let expected_after = last.unwrap_or(0) as u64;
        if record.seq != expected_after + 1 {
            return Err(StoreError::LogOutOfOrder { got: record.seq, expected_after });
        }
        conn.execute(
            "INSERT INTO signal_log (seq, created_at, payload, prev_hash, hash) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![record.seq as i64, record.created_at, record.payload, record.prev_hash, record.hash],
        )?;
        Ok(())
    }

    pub fn log(&self) -> Result<Vec<LogRecord>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT seq, created_at, payload, prev_hash, hash FROM signal_log ORDER BY seq")?;
        let rows = stmt.query_map([], |r| {
            Ok(LogRecord {
                seq: r.get::<_, i64>(0)? as u64,
                created_at: r.get(1)?,
                payload: r.get(2)?,
                prev_hash: r.get(3)?,
                hash: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn last_log(&self) -> Result<Option<LogRecord>> {
        Ok(self.log()?.pop())
    }
}

/// Equal apart from how the counterparty's name is spelled. "Acme Ltd" and
/// "Acme Ltd." are the same edge and must not undo an approval.
fn same_content(a: &Relation, b: &Relation) -> bool {
    let mut a = a.clone();
    a.counterparty.name = b.counterparty.name.clone();
    a == *b && normalize_name(&a.counterparty.name) == normalize_name(&b.counterparty.name)
}

fn collect(
    rows: impl Iterator<Item = rusqlite::Result<Result<StoredRelation>>>,
) -> Result<Vec<StoredRelation>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row??);
    }
    Ok(out)
}

fn cik_from(value: i64) -> Result<Cik> {
    u64::try_from(value)
        .ok()
        .and_then(|v| Cik::new(v).ok())
        .ok_or_else(|| StoreError::Corrupt(format!("cik {value}")))
}

fn company_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Company>> {
    let cik: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let ticker: Option<String> = row.get(2)?;
    Ok(cik_from(cik).map(|cik| Company { cik, name, ticker }))
}

fn relation_from_row(row: &Row<'_>) -> rusqlite::Result<Result<StoredRelation>> {
    let id: i64 = row.get(0)?;
    let subject: i64 = row.get(1)?;
    let name: String = row.get(2)?;
    let counterparty_cik: Option<i64> = row.get(3)?;
    let direction: String = row.get(4)?;
    let value: Option<f64> = row.get(5)?;
    let unit: Option<String> = row.get(6)?;
    let basis: Option<String> = row.get(7)?;
    let year: Option<i64> = row.get(8)?;
    let accession: Option<String> = row.get(9)?;
    let url: Option<String> = row.get(10)?;
    let quote: String = row.get(11)?;
    let confidence: String = row.get(12)?;
    let corroborated: bool = row.get(13)?;
    let status: String = row.get(14)?;
    let note: Option<String> = row.get(15)?;
    let known_at: i64 = row.get(16)?;
    let reviewed_at: Option<i64> = row.get(17)?;

    Ok((|| {
        let amount = match (value, unit) {
            (Some(value), Some(unit)) => Some(Amount { value, unit: parse_unit(&unit)?, basis }),
            (None, None) => None,
            _ => return Err(StoreError::Corrupt(format!("relation {id} has half an amount"))),
        };
        let period = match year {
            Some(y) => Some(Period {
                fiscal_year: u16::try_from(y).map_err(|_| StoreError::Corrupt(format!("year {y}")))?,
            }),
            None => None,
        };
        Ok(StoredRelation {
            id,
            relation: Relation {
                subject: cik_from(subject)?,
                counterparty: Counterparty {
                    name,
                    cik: counterparty_cik.map(cik_from).transpose()?,
                },
                direction: parse_direction(&direction)?,
                amount,
                period,
                evidence: Evidence { accession, url, quote },
                confidence: parse_confidence(&confidence)?,
                corroborated,
            },
            status: parse_status(&status)?,
            note,
            known_at,
            reviewed_at,
        })
    })())
}

fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Customer => "customer",
        Direction::Supplier => "supplier",
    }
}

fn parse_direction(s: &str) -> Result<Direction> {
    match s {
        "customer" => Ok(Direction::Customer),
        "supplier" => Ok(Direction::Supplier),
        other => Err(StoreError::Corrupt(format!("direction {other:?}"))),
    }
}

fn confidence_str(c: Confidence) -> &'static str {
    match c {
        Confidence::Disclosed => "disclosed",
        Confidence::Registry => "registry",
        Confidence::Named => "named",
        Confidence::Reverse => "reverse",
    }
}

fn parse_confidence(s: &str) -> Result<Confidence> {
    match s {
        "disclosed" => Ok(Confidence::Disclosed),
        "registry" => Ok(Confidence::Registry),
        "named" => Ok(Confidence::Named),
        "reverse" => Ok(Confidence::Reverse),
        other => Err(StoreError::Corrupt(format!("confidence {other:?}"))),
    }
}

fn unit_str(u: Unit) -> &'static str {
    match u {
        Unit::Percent => "percent",
        Unit::Usd => "usd",
    }
}

fn parse_unit(s: &str) -> Result<Unit> {
    match s {
        "percent" => Ok(Unit::Percent),
        "usd" => Ok(Unit::Usd),
        other => Err(StoreError::Corrupt(format!("unit {other:?}"))),
    }
}

fn status_str(s: ReviewStatus) -> &'static str {
    match s {
        ReviewStatus::Pending => "pending",
        ReviewStatus::Approved => "approved",
        ReviewStatus::Rejected => "rejected",
    }
}

fn parse_status(s: &str) -> Result<ReviewStatus> {
    match s {
        "pending" => Ok(ReviewStatus::Pending),
        "approved" => Ok(ReviewStatus::Approved),
        "rejected" => Ok(ReviewStatus::Rejected),
        other => Err(StoreError::Corrupt(format!("status {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cik(n: u64) -> Cik {
        Cik::new(n).unwrap()
    }

    fn relation(name: &str, direction: Direction, pct: Option<f64>) -> Relation {
        Relation {
            subject: cik(42),
            counterparty: Counterparty { name: name.into(), cik: None },
            direction,
            amount: pct.map(|value| Amount { value, unit: Unit::Percent, basis: Some("net sales".into()) }),
            period: Some(Period { fiscal_year: 2025 }),
            evidence: Evidence {
                accession: Some("0000000042-25-000004".into()),
                url: None,
                quote: format!("{name} is mentioned here"),
            },
            confidence: if pct.is_some() { Confidence::Disclosed } else { Confidence::Named },
            corroborated: false,
        }
    }

    #[test]
    fn company_round_trip_and_ticker_lookup() {
        let store = Store::open_in_memory().unwrap();
        let company = Company { cik: cik(42), name: "Nordvik Test Components Inc.".into(), ticker: Some("NTC".into()) };
        store.upsert_company(&company).unwrap();
        assert_eq!(store.company(cik(42)).unwrap(), Some(company.clone()));
        assert_eq!(store.company_by_ticker(" ntc ").unwrap(), Some(company));
        assert_eq!(store.company(cik(7)).unwrap(), None);
        assert_eq!(store.company_by_ticker("ZZZ").unwrap(), None);
    }

    #[test]
    fn relation_round_trips_exactly() {
        let store = Store::open_in_memory().unwrap();
        let mut r = relation("Haldenfjord Motors Corp", Direction::Customer, Some(21.0));
        r.counterparty.cik = Some(cik(10));
        r.corroborated = true;
        let id = store.upsert_relation_at(&r, 1000).unwrap().id();
        let stored = store.relation(id).unwrap().unwrap();
        assert_eq!(stored.relation, r);
        assert_eq!(stored.status, ReviewStatus::Pending);
        assert_eq!(stored.known_at, 1000);
        assert_eq!(stored.reviewed_at, None);
    }

    #[test]
    fn same_content_is_unchanged_and_keeps_approval() {
        let store = Store::open_in_memory().unwrap();
        let r = relation("Polarvik Castings Ltd", Direction::Supplier, None);
        let id = store.upsert_relation_at(&r, 1000).unwrap().id();
        store.set_review(id, ReviewStatus::Approved, Some("checked against the filing")).unwrap();

        // The same name with another suffix is the same edge.
        let mut again = r.clone();
        again.counterparty.name = "Polarvik Castings Ltd.".into();
        assert_eq!(store.upsert_relation_at(&again, 2000).unwrap(), Upsert::Unchanged(id));
        let stored = store.relation(id).unwrap().unwrap();
        assert_eq!(stored.status, ReviewStatus::Approved);
        assert_eq!(stored.known_at, 1000);
    }

    #[test]
    fn changed_content_goes_back_to_pending() {
        let store = Store::open_in_memory().unwrap();
        let r = relation("Haldenfjord Motors Corp", Direction::Customer, Some(21.0));
        let id = store.upsert_relation_at(&r, 1000).unwrap().id();
        store.set_review(id, ReviewStatus::Approved, None).unwrap();

        let mut newer = relation("Haldenfjord Motors Corporation", Direction::Customer, Some(24.0));
        newer.period = Some(Period { fiscal_year: 2026 });
        assert_eq!(store.upsert_relation_at(&newer, 2000).unwrap(), Upsert::Updated(id));
        let stored = store.relation(id).unwrap().unwrap();
        assert_eq!(stored.status, ReviewStatus::Pending);
        assert_eq!(stored.reviewed_at, None);
        assert_eq!(stored.known_at, 2000);
        assert_eq!(stored.relation.amount.as_ref().unwrap().value, 24.0);
        assert_eq!(store.all_relations().unwrap().len(), 1);
    }

    #[test]
    fn chain_splits_and_filters_by_status() {
        let store = Store::open_in_memory().unwrap();
        let a = store.upsert_relation(&relation("Small Buyer Inc", Direction::Customer, Some(5.0))).unwrap().id();
        let b = store.upsert_relation(&relation("Big Buyer Inc", Direction::Customer, Some(30.0))).unwrap().id();
        store.upsert_relation(&relation("Unnamed Share Buyer", Direction::Customer, None)).unwrap();
        store.upsert_relation(&relation("Polarvik Castings Ltd", Direction::Supplier, None)).unwrap();
        store.set_review(a, ReviewStatus::Approved, None).unwrap();
        store.set_review(b, ReviewStatus::Rejected, Some("wrong company")).unwrap();

        let all = store.chain(cik(42), None).unwrap();
        assert_eq!((all.customers.len(), all.suppliers.len()), (3, 1));
        // Largest share first, relations without an amount last.
        assert_eq!(all.customers[0].relation.counterparty.name, "Big Buyer Inc");
        assert_eq!(all.customers[2].relation.counterparty.name, "Unnamed Share Buyer");
        assert!(all.company.is_none());

        let approved = store.chain(cik(42), Some(ReviewStatus::Approved)).unwrap();
        assert_eq!((approved.customers.len(), approved.suppliers.len()), (1, 0));
        assert_eq!(store.relations_with_status(ReviewStatus::Pending).unwrap().len(), 2);
        assert_eq!(store.relation(b).unwrap().unwrap().note.as_deref(), Some("wrong company"));
    }

    #[test]
    fn review_of_missing_relation_is_an_error() {
        let store = Store::open_in_memory().unwrap();
        assert!(matches!(
            store.set_review(99, ReviewStatus::Approved, None),
            Err(StoreError::NoSuchRelation(99))
        ));
    }

    #[test]
    fn log_must_be_appended_in_order() {
        let store = Store::open_in_memory().unwrap();
        let rec = |seq| LogRecord { seq, created_at: 1, payload: "{}".into(), prev_hash: "p".into(), hash: "h".into() };
        assert!(matches!(store.append_log(&rec(2)), Err(StoreError::LogOutOfOrder { got: 2, expected_after: 0 })));
        store.append_log(&rec(1)).unwrap();
        store.append_log(&rec(2)).unwrap();
        assert!(store.append_log(&rec(2)).is_err());
        assert_eq!(store.log().unwrap().len(), 2);
        assert_eq!(store.last_log().unwrap().unwrap().seq, 2);
    }

    fn event(id: &str, time: i64, mag: f64) -> Event {
        Event {
            source: "usgs".into(),
            external_id: id.into(),
            kind: "earthquake".into(),
            title: format!("M {mag} somewhere"),
            time,
            lat: Some(1.0),
            lon: Some(2.0),
            severity: Some(mag),
            severity_unit: Some("magnitude".into()),
            alert: None,
            country: None,
            url: None,
        }
    }

    #[test]
    fn events_are_new_once_and_revisions_keep_first_known_at() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.upsert_events_at(&[event("a", 100, 5.0), event("b", 200, 6.0)], 1000).unwrap(), 2);
        // The source revises one magnitude and adds one event.
        assert_eq!(store.upsert_events_at(&[event("a", 100, 5.3), event("c", 300, 4.6)], 2000).unwrap(), 1);
        let events = store.events(10).unwrap();
        assert_eq!(events.iter().map(|e| e.external_id.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(events[2].severity, Some(5.3));
        let known: i64 = store
            .conn()
            .query_row("SELECT known_at FROM event WHERE external_id = 'a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(known, 1000);
        assert_eq!(store.events(1).unwrap().len(), 1);
        assert_eq!(store.event_count().unwrap(), 3);
    }

    #[test]
    fn series_counts_only_new_and_revised_points() {
        let store = Store::open_in_memory().unwrap();
        let p = |date: &str, value| SeriesPoint { date: date.into(), value };
        assert_eq!(store.upsert_series_at("ecb", "EXR.D.SEK.EUR", &[p("2026-11-20", 11.21), p("2026-11-19", 11.14)], 10).unwrap(), 2);
        assert_eq!(store.upsert_series_at("ecb", "EXR.D.SEK.EUR", &[p("2026-11-20", 11.21)], 20).unwrap(), 0);
        assert_eq!(store.upsert_series_at("ecb", "EXR.D.SEK.EUR", &[p("2026-11-20", 11.22), p("2026-11-23", 11.23)], 30).unwrap(), 2);
        let points = store.series("ecb", "EXR.D.SEK.EUR").unwrap();
        assert_eq!(points.iter().map(|p| p.value).collect::<Vec<_>>(), [11.14, 11.22, 11.23]);
        assert!(store.series("ecb", "other").unwrap().is_empty());
        assert_eq!(store.series_list().unwrap(), vec![("ecb".into(), "EXR.D.SEK.EUR".into(), 3, "2026-11-23".into())]);
    }

    #[test]
    fn sanctions_are_replaced_whole_and_matched_on_normalised_name() {
        let store = Store::open_in_memory().unwrap();
        let entry = |id, name: &str| SanctionEntry { list_id: id, name: name.into(), kind: None, program: Some("TEST".into()) };
        store.replace_sanctions("ofac", &[entry(1, "ANGLO-CARIBBEAN CO., LTD."), entry(2, "Old Entry Inc")]).unwrap();
        assert_eq!(store.sanction_matches("Anglo Caribbean Co Ltd").unwrap().len(), 1);
        assert_eq!(store.sanction_matches("anglo-caribbean").unwrap()[0].list_id, 1);
        assert!(store.sanction_matches("Caribbean").unwrap().is_empty(), "no partial matches");

        store.replace_sanctions("ofac", &[entry(1, "ANGLO-CARIBBEAN CO., LTD.")]).unwrap();
        assert_eq!(store.sanction_count().unwrap(), 1);
        assert!(store.sanction_matches("Old Entry").unwrap().is_empty(), "delisted names disappear");
    }

    #[test]
    fn locations_are_found_by_owner_whatever_the_suffix() {
        let store = Store::open_in_memory().unwrap();
        let place = |id: &str, owner: &str, lat| Location {
            owner: owner.into(),
            label: format!("plant {id}"),
            lat,
            lon: 10.0,
            source: "manual".into(),
            external_id: id.into(),
        };
        store.upsert_locations(&[place("a", "Polarvik Castings Ltd", 1.0), place("b", "Other Works Inc", 2.0)]).unwrap();
        assert_eq!(store.locations(Some("POLARVIK CASTINGS LIMITED")).unwrap().len(), 1);
        assert_eq!(store.locations(None).unwrap().len(), 2);
        // The same register row again replaces, it does not duplicate.
        store.upsert_locations(&[place("a", "Polarvik Castings Ltd", 5.0)]).unwrap();
        let again = store.locations(Some("Polarvik Castings")).unwrap();
        assert_eq!((again.len(), again[0].lat), (1, 5.0));
    }

    #[test]
    fn data_survives_reopening_the_file() {
        let dir = std::env::temp_dir().join(format!("term-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            store.upsert_relation(&relation("Polarvik Castings Ltd", Direction::Supplier, None)).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.all_relations().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

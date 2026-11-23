//! Free data sources beyond SEC EDGAR.
//!
//! Each module has an address builder, a parser and nothing else. The
//! parsers are tested against recorded real responses in
//! `fixtures/kallor/`. The status of each source:
//!
//! | Module | Source | Key | Status |
//! |---|---|---|---|
//! | `usgs` | USGS earthquake feed | none | run live 2026-11-23 |
//! | `gdacs` | GDACS disaster alerts | none | run live 2026-11-23 |
//! | `gleif` | GLEIF, legal entity identifiers | none | run live 2026-11-23 |
//! | `openfigi` | OpenFIGI, instrument identifiers | optional | run live without key 2026-11-23 |
//! | `ecb` | ECB reference exchange rates | none | run live 2026-11-23 |
//! | `riksbank` | Riksbanken, rates and exchange rates | none | run live 2026-11-23 |
//! | `worldbank` | World Bank indicators | none | run live 2026-11-23 |
//! | `esef` | filings.xbrl.org, European annual reports | none | run live 2026-11-23 |
//! | `ofac` | OFAC sanctions list | none | run live 2026-11-23 |
//! | `fred` | FRED, US rates and macro | free key | NOT run, no key |

pub mod ecb;
pub mod esef;
pub mod fred;
pub mod gdacs;
pub mod gleif;
pub mod ofac;
pub mod openfigi;
pub mod riksbank;
pub mod usgs;
pub mod worldbank;

use term_ingest::RateLimiter;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("http: {0}")]
    Http(String),
    #[error("{host} answered {status}")]
    Status { host: String, status: u16 },
    #[error("unexpected format: {0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, SourceError>;

/// Shared client for the free sources: identifies itself and never sends
/// more than two requests per second in total.
pub struct Http {
    client: reqwest::Client,
    limiter: RateLimiter,
}

impl Http {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("marknadsterminalen/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| SourceError::Http(e.to_string()))?;
        Ok(Self { client, limiter: RateLimiter::per_second(2) })
    }

    async fn finish(&self, url: &str, response: reqwest::Result<reqwest::Response>) -> Result<String> {
        // Only the host goes into error messages. Some addresses carry a key.
        let host = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "source".to_string());
        let response = response.map_err(|e| SourceError::Http(format!("{host}: {}", e.without_url())))?;
        let status = response.status();
        if !status.is_success() {
            return Err(SourceError::Status { host, status: status.as_u16() });
        }
        response.text().await.map_err(|e| SourceError::Http(format!("{host}: {}", e.without_url())))
    }

    pub async fn get_text(&self, url: &str) -> Result<String> {
        self.limiter.acquire().await;
        let response = self.client.get(url).send().await;
        self.finish(url, response).await
    }

    pub async fn post_json(&self, url: &str, body: &serde_json::Value, headers: &[(&str, &str)]) -> Result<String> {
        self.limiter.acquire().await;
        let mut request = self.client.post(url).json(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await;
        self.finish(url, response).await
    }
}

fn format_err(e: impl std::fmt::Display) -> SourceError {
    SourceError::Format(e.to_string())
}

/// Path segments and codes that go into an address must be plain.
fn plain_code(value: &str, what: &str) -> Result<String> {
    let v = value.trim();
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-') {
        return Err(SourceError::Config(format!("{what} {value:?} may only hold letters, digits, dot, dash and underscore")));
    }
    Ok(v.to_string())
}

#[cfg(test)]
pub(crate) fn fixture(name: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/kallor").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_cannot_smuggle_path_or_query() {
        assert_eq!(plain_code(" DGS10 ", "series").unwrap(), "DGS10");
        assert_eq!(plain_code("NY.GDP.MKTP.CD", "indicator").unwrap(), "NY.GDP.MKTP.CD");
        for bad in ["", "a/b", "a?b=c", "a b", "a&b", "../x"] {
            assert!(plain_code(bad, "series").is_err(), "{bad:?}");
        }
    }
}

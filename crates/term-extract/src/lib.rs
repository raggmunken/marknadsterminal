//! Chain engine steps 3 and 4: extract relations from passages and
//! throw away every relation whose quote is not in the source.

pub mod anthropic;
pub mod baseline;
pub mod chain;
pub mod claude_cli;
pub mod eval;
pub mod openai_compat;
pub mod quote;

use serde::{Deserialize, Serialize};
use term_core::{Amount, Direction};

/// What an extractor claims to have found. Nothing here is trusted until
/// [`quote::verify_quote`] has passed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRelation {
    pub counterparty: String,
    pub direction: Direction,
    pub amount: Option<Amount>,
    pub fiscal_year: Option<u16>,
    /// Text copied from the passage that supports the relation.
    pub quote: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("http: {0}")]
    Http(String),
    #[error("api answered {status}: {body}")]
    Api { status: u16, body: String },
    #[error("malformed model output: {0}")]
    Malformed(String),
}

/// Step 3. One passage in, zero or more claimed relations out.
#[allow(async_fn_in_trait)]
pub trait Extractor {
    /// Short name for logs and evaluation tables.
    fn name(&self) -> &str;

    async fn extract(&self, passage: &str) -> Result<Vec<RawRelation>, ExtractError>;

    /// Whether the extractor can read free text, so that it is shown more paragraphs
    /// than the fixed phrase patterns need. False for the rule based extractor.
    fn reads_free_text(&self) -> bool {
        false
    }
}

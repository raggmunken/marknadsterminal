//! Extraction with a language model over the Anthropic Messages API.
//!
//! STATUS: building the request and reading the answer are unit tested
//! against a canned response. The live call in [`AnthropicExtractor`] has
//! never been run, because no API key was available when this was written.
//! Treat it as unproven until the prototype week has run it on real filings.
//!
//! The plan names `rig-core` for this job. The prototype talks to the API
//! directly to keep the dependency list short. Phase 0 decides which stays.

use crate::{ExtractError, Extractor, RawRelation};
use serde::Deserialize;
use serde_json::{json, Value};
use term_core::{Amount, Direction, Unit};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
/// Overridden by the environment variable `TERM_LLM_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
const TOOL_NAME: &str = "record_relations";

const SYSTEM: &str = "You extract supply chain relations from a passage of a company's annual report. \
Report only relations that the passage states about the reporting company: a named customer it sells to, \
or a named supplier it buys from. Never use outside knowledge. Skip unnamed parties such as 'Customer A'. \
For every relation, copy a quote of at least 20 characters from the passage, character for character. \
A relation whose quote is not found in the passage is discarded. If the passage states no such relation, \
return an empty list.";

/// The request body for one passage. Pure, so that it can be tested.
pub fn build_request(model: &str, passage: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 2048,
        "system": SYSTEM,
        "tools": [{
            "name": TOOL_NAME,
            "description": "Record the customer and supplier relations stated in the passage.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "relations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "counterparty": {"type": "string", "description": "Name as written in the passage"},
                                "direction": {"type": "string", "enum": ["customer", "supplier"]},
                                "amount_value": {"type": ["number", "null"]},
                                "amount_unit": {"type": ["string", "null"], "enum": ["percent", "usd", null]},
                                "amount_basis": {"type": ["string", "null"], "description": "For example 'net sales'"},
                                "fiscal_year": {"type": ["integer", "null"]},
                                "quote": {"type": "string", "description": "Copied verbatim from the passage"}
                            },
                            "required": ["counterparty", "direction", "quote"]
                        }
                    }
                },
                "required": ["relations"]
            }
        }],
        "tool_choice": {"type": "tool", "name": TOOL_NAME},
        "messages": [{
            "role": "user",
            "content": format!("<passage>\n{passage}\n</passage>")
        }]
    })
}

#[derive(Debug, Deserialize)]
struct ToolInput {
    relations: Vec<LlmRelation>,
}

#[derive(Debug, Deserialize)]
struct LlmRelation {
    counterparty: String,
    direction: Direction,
    #[serde(default)]
    amount_value: Option<f64>,
    #[serde(default)]
    amount_unit: Option<Unit>,
    #[serde(default)]
    amount_basis: Option<String>,
    #[serde(default)]
    fiscal_year: Option<u16>,
    quote: String,
}

impl From<LlmRelation> for RawRelation {
    fn from(r: LlmRelation) -> Self {
        // An amount needs both a value and a unit. Half an amount is dropped.
        let amount = match (r.amount_value, r.amount_unit) {
            (Some(value), Some(unit)) => Some(Amount { value, unit, basis: r.amount_basis }),
            _ => None,
        };
        RawRelation {
            counterparty: r.counterparty.trim().to_string(),
            direction: r.direction,
            amount,
            fiscal_year: r.fiscal_year,
            quote: r.quote,
        }
    }
}

/// Read the tool call out of a Messages API response. Pure.
pub fn parse_response(body: &Value) -> Result<Vec<RawRelation>, ExtractError> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ExtractError::Malformed("response has no content array".into()))?;
    let input = blocks
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")
            && b.get("name").and_then(Value::as_str) == Some(TOOL_NAME))
        .and_then(|b| b.get("input"))
        .ok_or_else(|| ExtractError::Malformed(format!("no {TOOL_NAME} tool call in response")))?;
    let parsed: ToolInput = serde_json::from_value(input.clone())
        .map_err(|e| ExtractError::Malformed(e.to_string()))?;
    Ok(parsed.relations.into_iter().map(RawRelation::from).collect())
}

pub struct AnthropicExtractor {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl AnthropicExtractor {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), api_key: api_key.into(), model: model.into() }
    }

    /// Reads `ANTHROPIC_API_KEY` and, if set, `TERM_LLM_MODEL`.
    pub fn from_env() -> Result<Self, ExtractError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| ExtractError::Config("ANTHROPIC_API_KEY is not set".into()))?;
        let model = std::env::var("TERM_LLM_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Ok(Self::new(api_key, model))
    }
}

impl Extractor for AnthropicExtractor {
    fn name(&self) -> &str {
        &self.model
    }

    async fn extract(&self, passage: &str) -> Result<Vec<RawRelation>, ExtractError> {
        let response = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&build_request(&self.model, passage))
            .send()
            .await
            .map_err(|e| ExtractError::Http(e.to_string()))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| ExtractError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(ExtractError::Api { status: status.as_u16(), body: text });
        }
        let body: Value =
            serde_json::from_str(&text).map_err(|e| ExtractError::Malformed(e.to_string()))?;
        parse_response(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_forces_the_tool_and_carries_the_passage() {
        let req = build_request("some-model", "Example passage text.");
        assert_eq!(req["model"], "some-model");
        assert_eq!(req["tool_choice"]["type"], "tool");
        assert_eq!(req["tool_choice"]["name"], TOOL_NAME);
        assert_eq!(req["tools"][0]["name"], TOOL_NAME);
        assert!(req["messages"][0]["content"].as_str().unwrap().contains("Example passage text."));
    }

    #[test]
    fn parses_tool_call() {
        let body = json!({
            "content": [
                {"type": "text", "text": "ignored"},
                {"type": "tool_use", "id": "x", "name": TOOL_NAME, "input": {"relations": [
                    {"counterparty": " Example Motors Corp. ", "direction": "customer",
                     "amount_value": 21, "amount_unit": "percent", "amount_basis": "net sales",
                     "fiscal_year": 2025, "quote": "Example Motors Corp. accounted for 21% of our net sales"},
                    {"counterparty": "Polar Castings Ltd", "direction": "supplier",
                     "amount_value": null, "amount_unit": null,
                     "quote": "housings are supplied by Polar Castings Ltd"}
                ]}}
            ]
        });
        let relations = parse_response(&body).unwrap();
        assert_eq!(relations.len(), 2);
        assert_eq!(relations[0].counterparty, "Example Motors Corp.");
        assert_eq!(relations[0].amount.as_ref().unwrap().value, 21.0);
        assert_eq!(relations[0].fiscal_year, Some(2025));
        assert_eq!(relations[1].direction, Direction::Supplier);
        assert!(relations[1].amount.is_none());
    }

    #[test]
    fn half_an_amount_is_dropped() {
        let body = json!({"content": [{"type": "tool_use", "name": TOOL_NAME, "input": {"relations": [
            {"counterparty": "X Corp", "direction": "customer", "amount_value": 5, "quote": "q"}
        ]}}]});
        assert!(parse_response(&body).unwrap()[0].amount.is_none());
    }

    #[test]
    fn empty_list_is_fine() {
        let body = json!({"content": [{"type": "tool_use", "name": TOOL_NAME, "input": {"relations": []}}]});
        assert!(parse_response(&body).unwrap().is_empty());
    }

    #[test]
    fn missing_tool_call_is_an_error() {
        let body = json!({"content": [{"type": "text", "text": "I could not find any."}]});
        assert!(matches!(parse_response(&body), Err(ExtractError::Malformed(_))));
        assert!(matches!(parse_response(&json!({})), Err(ExtractError::Malformed(_))));
    }

    #[test]
    fn unknown_direction_is_an_error() {
        let body = json!({"content": [{"type": "tool_use", "name": TOOL_NAME, "input": {"relations": [
            {"counterparty": "X Corp", "direction": "partner", "quote": "q"}
        ]}}]});
        assert!(matches!(parse_response(&body), Err(ExtractError::Malformed(_))));
    }
}

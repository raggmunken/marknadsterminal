//! Extraction with any service that speaks the OpenAI chat completions format.
//!
//! Settings come from the environment, so that no key is ever written to a file:
//! `TERM_LLM_BASE_URL`, `TERM_LLM_API_KEY` and `TERM_LLM_MODEL`. Every quote is
//! still checked against the source by the chain.

use crate::anthropic::system_prompt;
use crate::claude_cli::{parse_answer, FORMAT};
use crate::{ExtractError, Extractor, RawRelation};
use serde_json::{json, Value};

pub struct OpenAiCompatExtractor {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiCompatExtractor {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    pub fn from_env() -> Result<Self, ExtractError> {
        let get = |k: &str| std::env::var(k).map_err(|_| ExtractError::Config(format!("{k} is not set")));
        Ok(Self::new(get("TERM_LLM_BASE_URL")?, get("TERM_LLM_API_KEY")?, get("TERM_LLM_MODEL")?))
    }
}

pub fn build_request(model: &str, passage: &str) -> Value {
    json!({
        "model": model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": format!("{} {FORMAT}", system_prompt())},
            {"role": "user", "content": format!("<passage>\n{passage}\n</passage>")}
        ]
    })
}

/// Read the answer text out of a chat completions response. Pure.
pub fn parse_response(body: &Value) -> Result<Vec<RawRelation>, ExtractError> {
    let text = body
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| ExtractError::Malformed("response has no choices[0].message.content".into()))?;
    parse_answer(text)
}

impl Extractor for OpenAiCompatExtractor {
    fn name(&self) -> &str {
        &self.model
    }

    async fn extract(&self, passage: &str) -> Result<Vec<RawRelation>, ExtractError> {
        let response = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&build_request(&self.model, passage))
            .send()
            .await
            .map_err(|e| ExtractError::Http(e.to_string()))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| ExtractError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(ExtractError::Api { status: status.as_u16(), body: text });
        }
        let body: Value = serde_json::from_str(&text).map_err(|e| ExtractError::Malformed(e.to_string()))?;
        parse_response(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_carries_model_system_and_passage() {
        let r = build_request("m", "Some passage.");
        assert_eq!(r["model"], "m");
        assert_eq!(r["messages"][0]["role"], "system");
        assert!(r["messages"][1]["content"].as_str().unwrap().contains("Some passage."));
    }

    #[test]
    fn reads_answer_from_choices() {
        let body = json!({"choices": [{"message": {"content": "```json\n{\"relations\": [{\"counterparty\": \"Magna\", \"direction\": \"supplier\", \"quote\": \"supplied by Magna International\"}]}\n```"}}]});
        let found = parse_response(&body).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].counterparty, "Magna");
    }

    #[test]
    fn missing_content_is_malformed() {
        assert!(matches!(parse_response(&json!({"choices": []})), Err(ExtractError::Malformed(_))));
    }
}

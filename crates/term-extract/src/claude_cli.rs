//! Extraction with a language model through the `claude` command line program.
//!
//! This uses the person's own signed-in Claude Code, so no API key is needed.
//! The program is started once per passage with the passage on standard input
//! and asked to answer with JSON only. Every quote is still checked against the
//! source by the chain, so a wrong answer from the model is discarded.
//!
//! STATUS: the parsing is unit tested and the process handling is tested
//! against a stand-in program. A live call needs a signed-in `claude`, which
//! the build sandbox did not have. Unproven against the real program.

use crate::anthropic::{parse_relations_value, system_prompt};
use crate::{ExtractError, Extractor, RawRelation};
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Overridden by `TERM_LLM_MODEL`.
pub const DEFAULT_CLI_MODEL: &str = "claude-haiku-5-5";

pub const FORMAT: &str = "Answer with one JSON object and nothing else, no code fence, in this form: \
{\"relations\": [{\"counterparty\": string, \"direction\": \"customer\" or \"supplier\", \
\"amount_value\": number or null, \"amount_unit\": \"percent\" or \"usd\" or null, \
\"amount_basis\": string or null, \"fiscal_year\": integer or null, \"quote\": string}]}. \
Use {\"relations\": []} when the passage states no relation.";

pub struct ClaudeCliExtractor {
    program: String,
    model: String,
}

impl ClaudeCliExtractor {
    pub fn new(program: impl Into<String>, model: impl Into<String>) -> Self {
        Self { program: program.into(), model: model.into() }
    }

    /// Reads `TERM_CLAUDE_BIN` (default `claude`) and `TERM_LLM_MODEL`.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var("TERM_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()),
            std::env::var("TERM_LLM_MODEL").unwrap_or_else(|_| DEFAULT_CLI_MODEL.into()),
        )
    }
}

/// Read the program's `--output-format json` envelope and the JSON the model wrote in it. Pure.
pub fn parse_output(stdout: &str) -> Result<Vec<RawRelation>, ExtractError> {
    let envelope: Value =
        serde_json::from_str(stdout.trim()).map_err(|e| ExtractError::Malformed(format!("envelope: {e}")))?;
    let result = envelope
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| ExtractError::Malformed("envelope has no result text".into()))?;
    if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(ExtractError::Api { status: 0, body: result.to_string() });
    }
    parse_answer(result)
}

/// The model may wrap the object in a code fence or a sentence. Take the outermost braces. Pure.
pub fn parse_answer(result: &str) -> Result<Vec<RawRelation>, ExtractError> {
    let (start, end) = (result.find('{'), result.rfind('}'));
    let (Some(start), Some(end)) = (start, end) else {
        return Err(ExtractError::Malformed("no JSON object in the answer".into()));
    };
    let value: Value = serde_json::from_str(&result[start..=end])
        .map_err(|e| ExtractError::Malformed(format!("answer: {e}")))?;
    parse_relations_value(&value)
}

impl Extractor for ClaudeCliExtractor {
    fn name(&self) -> &str {
        &self.model
    }

    fn reads_free_text(&self) -> bool {
        true
    }

    async fn extract(&self, passage: &str) -> Result<Vec<RawRelation>, ExtractError> {
        let system = format!("{} {FORMAT}", system_prompt());
        let mut child = Command::new(&self.program)
            .args(["-p", "--model", &self.model, "--output-format", "json", "--no-session-persistence", "--tools", "", "--system-prompt", &system])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ExtractError::Config(format!("could not start {}: {e}", self.program)))?;
        let mut stdin = child.stdin.take().ok_or_else(|| ExtractError::Config("no stdin".into()))?;
        let input = format!("<passage>\n{passage}\n</passage>");
        stdin.write_all(input.as_bytes()).await.map_err(|e| ExtractError::Http(e.to_string()))?;
        drop(stdin);
        let out = child.wait_with_output().await.map_err(|e| ExtractError::Http(e.to_string()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            return Err(ExtractError::Api { status: out.status.code().unwrap_or(-1) as u16, body: String::from_utf8_lossy(&out.stderr).into_owned() });
        }
        parse_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(result: &str, is_error: bool) -> String {
        json!({"type": "result", "is_error": is_error, "result": result}).to_string()
    }

    #[test]
    fn reads_plain_json_answer() {
        let answer = r#"{"relations": [{"counterparty": "Magna", "direction": "supplier", "quote": "supplied by Magna International"}]}"#;
        let found = parse_output(&envelope(answer, false)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].counterparty, "Magna");
    }

    #[test]
    fn reads_answer_inside_code_fence() {
        let answer = "```json\n{\"relations\": []}\n```";
        assert!(parse_output(&envelope(answer, false)).unwrap().is_empty());
    }

    #[test]
    fn error_envelope_is_an_error() {
        let err = parse_output(&envelope("Authentication error", true)).unwrap_err();
        assert!(matches!(err, ExtractError::Api { .. }), "{err:?}");
    }

    #[test]
    fn prose_without_json_is_malformed() {
        assert!(matches!(parse_output(&envelope("I cannot help.", false)), Err(ExtractError::Malformed(_))));
    }

    #[tokio::test]
    async fn runs_a_stand_in_program() {
        let dir = std::env::temp_dir().join(format!("fakeclaude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("claude");
        let body = r#"{"type":"result","is_error":false,"result":"{\"relations\": [{\"counterparty\": \"Polar Castings Ltd\", \"direction\": \"supplier\", \"quote\": \"supplied by Polar Castings Ltd\"}]}"}"#;
        std::fs::write(&script, format!("#!/bin/sh\ncat > /dev/null\necho '{body}'\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let ex = ClaudeCliExtractor::new(script.to_string_lossy(), "test-model");
        let found = ex.extract("Housings are supplied by Polar Castings Ltd.").await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].counterparty, "Polar Castings Ltd");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

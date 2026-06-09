//! Test-only fakes shared across module-level `#[cfg(test)]` suites.
//!
//! Keeps the fakes in one place so tests in `agent`, `tools`, and
//! `pob_parser` can stay focused on the behavior under test.

use std::sync::{Arc, Mutex};

use async_stream::stream;

use crate::llm::{LlmClient, LlmError, ResponseStream, ResponseStreamEvent, ToolDefinition};
use crate::pob_parser::{PobBackend, PobParseError, PobParser, PobQuery};

/// A recorded LLM call, captured by [`FakeLlm`] for later assertions.
#[derive(Debug, Clone)]
pub struct CapturedCall {
    /// Was `instructions` populated? (First round passes the system prompt;
    /// subsequent rounds don't.)
    pub had_instructions: bool,
    /// Whether tools were passed on this call.
    #[allow(dead_code)]
    pub had_tools: bool,
    /// `previous_response_id`, if any — set once response chaining starts.
    pub previous_response_id: Option<String>,
    /// Number of input items (messages / tool-results) on this call.
    pub input_len: usize,
}

/// Script for a single round of the agent's ReAct loop: a sequence of
/// events that the fake LLM will yield when it's asked to stream a response.
///
/// A "round" ends with a `ResponseCompleted` event; the agent loop then
/// decides whether to loop (if the round produced function calls) or
/// terminate (if it produced only text).
pub type Round = Vec<Result<ResponseStreamEvent, LlmError>>;

/// Fake [`LlmClient`] that plays scripted rounds.
///
/// Construct with [`FakeLlm::scripted`] and pass a sequence of rounds.
/// On each call to [`LlmClient::stream_response`], the fake pops the next
/// round from the script and yields its events. If the script is exhausted
/// it yields an error, which surfaces the bug in the calling test.
#[derive(Clone)]
pub struct FakeLlm {
    script: Arc<Mutex<Vec<Round>>>,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
}

impl FakeLlm {
    /// Create a fake from a list of rounds (first round is played first).
    pub fn scripted(rounds: Vec<Round>) -> Self {
        let mut script = rounds;
        script.reverse(); // so pop() returns the next round
        Self {
            script: Arc::new(Mutex::new(script)),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Return a clone of all captured calls (in order).
    pub fn captured(&self) -> Vec<CapturedCall> {
        self.calls.lock().unwrap().clone()
    }

    /// How many rounds have been played.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl LlmClient for FakeLlm {
    fn stream_response(
        &self,
        input: &[serde_json::Value],
        instructions: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        previous_response_id: Option<&str>,
    ) -> ResponseStream {
        // Record this call for later assertions.
        self.calls.lock().unwrap().push(CapturedCall {
            had_instructions: instructions.is_some(),
            had_tools: tools.is_some(),
            previous_response_id: previous_response_id.map(str::to_owned),
            input_len: input.len(),
        });

        let round = self.script.lock().unwrap().pop().unwrap_or_else(|| {
            // No scripted round available — yield a clear error so the
            // test fails with a helpful message rather than hanging.
            vec![Err(LlmError::Other(anyhow::anyhow!(
                "FakeLlm script exhausted — agent made more LLM calls than expected"
            )))]
        });

        Box::pin(stream! {
            for event in round {
                yield event;
            }
        })
    }
}

// -- Fake PoB backend --------------------------------------------------------

/// Closure type used to stub out a single query from the fake backend.
///
/// Receives the query and returns either a JSON response or a parse error.
pub type QueryFn = Arc<dyn Fn(&PobQuery) -> Result<serde_json::Value, PobParseError> + Send + Sync>;

/// A minimal fake [`PobBackend`] that returns canned data. Use
/// [`FakeBackend::with_query_fn`] to customize query responses, or rely on
/// the default (which returns an empty JSON object for every query).
pub struct FakeBackend {
    query_fn: QueryFn,
}

impl FakeBackend {
    /// Fake that returns `{}` for every query and succeeds for `load`/`calc`.
    pub fn default_ok() -> Self {
        Self {
            query_fn: Arc::new(|_| Ok(serde_json::json!({}))),
        }
    }

    /// Fake that invokes `f` to produce a response for each query.
    pub fn with_query_fn<F>(f: F) -> Self
    where
        F: Fn(&PobQuery) -> Result<serde_json::Value, PobParseError> + Send + Sync + 'static,
    {
        Self {
            query_fn: Arc::new(f),
        }
    }
}

impl PobBackend for FakeBackend {
    fn load_build_xml(&self, _xml: &str) -> Result<(), PobParseError> {
        Ok(())
    }

    fn calculate_json(&self) -> Result<Vec<u8>, PobParseError> {
        Ok(b"{}".to_vec())
    }

    fn query(&self, query: &PobQuery) -> Result<serde_json::Value, PobParseError> {
        (self.query_fn)(query)
    }
}

/// Spawn a [`PobParser`] backed by [`FakeBackend::default_ok`].
pub async fn fake_parser() -> PobParser {
    PobParser::new_with_backend(|| Ok(Box::new(FakeBackend::default_ok()) as Box<dyn PobBackend>))
        .await
        .expect("fake parser should always init")
}

/// Spawn a [`PobParser`] with a custom query handler.
pub async fn fake_parser_with<F>(f: F) -> PobParser
where
    F: Fn(&PobQuery) -> Result<serde_json::Value, PobParseError> + Send + Sync + 'static,
{
    PobParser::new_with_backend(move || {
        Ok(Box::new(FakeBackend::with_query_fn(f)) as Box<dyn PobBackend>)
    })
    .await
    .expect("fake parser should always init")
}

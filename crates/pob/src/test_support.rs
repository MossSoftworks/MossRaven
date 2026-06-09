//! Test-only fakes shared across module-level `#[cfg(test)]` suites.
//!
//! Trimmed from the original poe2-agent fork: that crate had an agent ReAct
//! loop and a `FakeLlm` mock, neither of which we kept in MossRaven (Tier 1
//! lives in `mossraven-dreamer`, not in pob). The only fake we still use is
//! [`FakeBackend`] for the [`PobParser`] threading + error-surfacing tests.

use std::sync::Arc;

use crate::parser::{PobBackend, PobParseError, PobParser, PobQuery};

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

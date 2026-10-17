//! P-3, the evidence page: the deployment manifest and the real transaction ids of every flow, from
//! the JSON file the integration owner fills (schema `zecured-evidence/1`, defined in
//! app/lib/evidence.ts, which parses it strictly). `GET /evidence` serves it as it is, read at most
//! once a minute so the file can be updated without a restart, after a structural check: the
//! schema, a `sections` list, at most [`MAX_BYTES`]. Only disposable demo vaults belong in it:
//! everything listed there is public on purpose.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{Json, http::StatusCode, response::IntoResponse, response::Response};
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// The schema the file must name.
pub const SCHEMA: &str = "zecured-evidence/1";
/// The largest file served.
pub const MAX_BYTES: u64 = 1 << 20;
/// How long one reading of the file is served.
const TTL: Duration = Duration::from_secs(60);

/// Why there is nothing to serve: a fixed answer each (L-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceError {
    /// No file configured, or none at its path.
    Missing,
    /// The file is not a `zecured-evidence/1` document.
    Invalid,
}

impl IntoResponse for EvidenceError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Missing => (StatusCode::NOT_FOUND, "not_configured", "No evidence is published on this backend."),
            Self::Invalid => (StatusCode::SERVICE_UNAVAILABLE, "evidence_invalid", "The evidence file is malformed."),
        };
        (status, Json(json!({ "error": message, "code": code }))).into_response()
    }
}

/// Checks a document's structure: the schema and a list of sections with ids.
pub fn check(doc: &Value) -> Result<(), EvidenceError> {
    let schema_ok = doc.get("schema").and_then(Value::as_str) == Some(SCHEMA);
    let sections_ok = doc
        .get("sections")
        .and_then(Value::as_array)
        .is_some_and(|s| s.iter().all(|x| x.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())));
    if schema_ok && sections_ok {
        Ok(())
    } else {
        Err(EvidenceError::Invalid)
    }
}

/// The evidence file at `path` (`ZECURED_EVIDENCE`), cached for [`TTL`].
#[derive(Clone)]
pub struct EvidenceFile {
    path: Option<PathBuf>,
    cache: Arc<Mutex<Option<(Instant, Result<Value, EvidenceError>)>>>,
}

impl EvidenceFile {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            cache: Arc::default(),
        }
    }

    fn read(&self) -> Result<Value, EvidenceError> {
        let path = self.path.as_ref().ok_or(EvidenceError::Missing)?;
        let meta = std::fs::metadata(path).map_err(|_| EvidenceError::Missing)?;
        if meta.len() > MAX_BYTES {
            return Err(EvidenceError::Invalid);
        }
        let raw = std::fs::read(path).map_err(|_| EvidenceError::Missing)?;
        let doc: Value = serde_json::from_slice(&raw).map_err(|_| EvidenceError::Invalid)?;
        check(&doc)?;
        Ok(doc)
    }

    /// The document, or why there is none; read again once the cached reading is [`TTL`] old.
    pub async fn get(&self) -> Result<Value, EvidenceError> {
        let mut cache = self.cache.lock().await;
        if let Some((at, value)) = cache.as_ref()
            && at.elapsed() < TTL
        {
            return value.clone();
        }
        let value = self.read();
        *cache = Some((Instant::now(), value.clone()));
        value
    }
}

/// `GET /evidence`.
pub async fn handler(axum::Extension(file): axum::Extension<EvidenceFile>) -> Result<Json<Value>, EvidenceError> {
    file.get().await.map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zecured-evidence-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("evidence.json")
    }

    /// The app's sample (app/lib/__fixtures__/evidence.sample.json) passes the backend's check.
    #[tokio::test]
    async fn the_apps_sample_is_served_as_it_is() {
        let sample = include_str!("../../app/lib/__fixtures__/evidence.sample.json");
        let path = temp("sample");
        std::fs::write(&path, sample).unwrap();
        let file = EvidenceFile::new(Some(path.clone()));
        let doc = file.get().await.unwrap();
        assert_eq!(doc, serde_json::from_str::<Value>(sample).unwrap());
        assert_eq!(doc["sections"][0]["id"], "judge");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn missing_or_malformed_files_are_fixed_answers() {
        assert_eq!(EvidenceFile::new(None).get().await, Err(EvidenceError::Missing));
        assert_eq!(
            EvidenceFile::new(Some(temp("none").with_file_name("absent.json"))).get().await,
            Err(EvidenceError::Missing)
        );
        for (name, body) in [
            ("schema", r#"{"schema":"zecured-evidence/2","sections":[]}"#),
            ("sections", r#"{"schema":"zecured-evidence/1"}"#),
            ("id", r#"{"schema":"zecured-evidence/1","sections":[{"id":""}]}"#),
            ("json", "not json"),
        ] {
            let path = temp(name);
            std::fs::write(&path, body).unwrap();
            assert_eq!(EvidenceFile::new(Some(path.clone())).get().await, Err(EvidenceError::Invalid), "{name}");
            let _ = std::fs::remove_file(path);
        }
        let big = temp("big");
        std::fs::write(&big, vec![b' '; (MAX_BYTES + 1) as usize]).unwrap();
        assert_eq!(EvidenceFile::new(Some(big.clone())).get().await, Err(EvidenceError::Invalid));
        let _ = std::fs::remove_file(big);
    }

    #[tokio::test]
    async fn readings_are_cached() {
        let path = temp("cache");
        std::fs::write(&path, r#"{"schema":"zecured-evidence/1","sections":[{"id":"judge"}]}"#).unwrap();
        let file = EvidenceFile::new(Some(path.clone()));
        assert!(file.get().await.is_ok());
        std::fs::write(&path, "broken").unwrap();
        assert!(file.get().await.is_ok(), "served from the cache for a minute");
        let _ = std::fs::remove_file(path);
    }
}

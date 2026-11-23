//! HTTP interface to the store, for the interface and for other tools.
//!
//! There is no login. Bind it to localhost, which is the default in the
//! command line, until authentication exists.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use term_core::{Cik, ReviewStatus};
use term_store::{Store, StoreError};

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: message.into() }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: message.into() }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NoSuchRelation(id) => ApiError::not_found(format!("relation {id} does not exist")),
            other => ApiError { status: StatusCode::INTERNAL_SERVER_ERROR, message: other.to_string() },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/companies/{cik}", get(company))
        .route("/companies/{cik}/chain", get(chain))
        .route("/companies/{cik}/analysis", get(analysis))
        .route("/relations/{id}", get(relation))
        .route("/relations/{id}/review", post(review))
        .route("/review/pending", get(pending))
        .with_state(store)
}

pub async fn serve(addr: SocketAddr, store: Arc<Store>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(store)).await
}

fn parse_cik(raw: &str) -> Result<Cik, ApiError> {
    raw.parse().map_err(|_| ApiError::bad_request(format!("{raw:?} is not a CIK")))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value)
        .map_err(|e| ApiError { status: StatusCode::INTERNAL_SERVER_ERROR, message: e.to_string() })
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn company(State(store): State<Arc<Store>>, Path(cik): Path<String>) -> ApiResult {
    let cik = parse_cik(&cik)?;
    match store.company(cik)? {
        Some(c) => Ok(Json(to_json(&c)?)),
        None => Err(ApiError::not_found(format!("company {cik} is not in the directory"))),
    }
}

#[derive(Deserialize)]
struct ChainQuery {
    status: Option<String>,
}

/// `?status=approved` is the default, so that a caller who forgets the
/// parameter never builds on relations nobody has reviewed.
fn parse_filter(raw: Option<&str>) -> Result<Option<ReviewStatus>, ApiError> {
    match raw.unwrap_or("approved") {
        "approved" => Ok(Some(ReviewStatus::Approved)),
        "pending" => Ok(Some(ReviewStatus::Pending)),
        "rejected" => Ok(Some(ReviewStatus::Rejected)),
        "all" => Ok(None),
        other => Err(ApiError::bad_request(format!(
            "status {other:?} is not one of approved, pending, rejected, all"
        ))),
    }
}

async fn chain(
    State(store): State<Arc<Store>>,
    Path(cik): Path<String>,
    Query(query): Query<ChainQuery>,
) -> ApiResult {
    let cik = parse_cik(&cik)?;
    let filter = parse_filter(query.status.as_deref())?;
    let view = store.chain(cik, filter)?;
    Ok(Json(json!({
        "cik": cik,
        "company": to_json(&view.company)?,
        "suppliers": to_json(&view.suppliers)?,
        "customers": to_json(&view.customers)?,
    })))
}

/// Everything the terminal can say about one company. Includes relations
/// that are not reviewed yet, and says so in its findings.
async fn analysis(State(store): State<Arc<Store>>, Path(cik): Path<String>) -> ApiResult {
    let cik = parse_cik(&cik)?;
    Ok(Json(to_json(&term_analysis::analyze_company(&store, cik)?)?))
}

async fn relation(State(store): State<Arc<Store>>, Path(id): Path<i64>) -> ApiResult {
    match store.relation(id)? {
        Some(r) => Ok(Json(to_json(&r)?)),
        None => Err(ApiError::not_found(format!("relation {id} does not exist"))),
    }
}

#[derive(Deserialize)]
struct ReviewBody {
    status: ReviewStatus,
    note: Option<String>,
}

async fn review(
    State(store): State<Arc<Store>>,
    Path(id): Path<i64>,
    Json(body): Json<ReviewBody>,
) -> ApiResult {
    let updated = store.set_review(id, body.status, body.note.as_deref())?;
    Ok(Json(to_json(&updated)?))
}

async fn pending(State(store): State<Arc<Store>>) -> ApiResult {
    Ok(Json(to_json(&store.relations_with_status(ReviewStatus::Pending)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use term_core::{Company, Confidence, Counterparty, Direction, Evidence, Relation};
    use tower::ServiceExt;

    fn seeded() -> (Arc<Store>, i64, i64) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let cik = Cik::new(42).unwrap();
        store
            .upsert_company(&Company { cik, name: "Nordvik Test Components Inc.".into(), ticker: Some("NTC".into()) })
            .unwrap();
        let rel = |name: &str, direction| Relation {
            subject: cik,
            counterparty: Counterparty { name: name.into(), cik: None },
            direction,
            amount: None,
            period: None,
            evidence: Evidence { accession: None, url: None, quote: format!("{name} in a made up sentence") },
            confidence: Confidence::Named,
            corroborated: false,
        };
        let a = store.upsert_relation(&rel("Haldenfjord Motors Corp", Direction::Customer)).unwrap().id();
        let b = store.upsert_relation(&rel("Polarvik Castings Ltd", Direction::Supplier)).unwrap().id();
        store.set_review(a, ReviewStatus::Approved, None).unwrap();
        (store, a, b)
    }

    async fn call(store: &Arc<Store>, request: Request<Body>) -> (StatusCode, Value) {
        let response = router(Arc::clone(store)).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn health_answers() {
        let (store, _, _) = seeded();
        let (status, body) = call(&store, get_req("/health")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn company_lookup_accepts_any_cik_spelling() {
        let (store, _, _) = seeded();
        for uri in ["/companies/42", "/companies/0000000042", "/companies/CIK0000000042"] {
            let (status, body) = call(&store, get_req(uri)).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(body["ticker"], "NTC");
        }
        assert_eq!(call(&store, get_req("/companies/7")).await.0, StatusCode::NOT_FOUND);
        let (status, body) = call(&store, get_req("/companies/abc")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("abc"));
    }

    #[tokio::test]
    async fn chain_shows_only_approved_unless_asked() {
        let (store, _, _) = seeded();
        let (status, body) = call(&store, get_req("/companies/42/chain")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["customers"].as_array().unwrap().len(), 1);
        assert_eq!(body["suppliers"].as_array().unwrap().len(), 0);
        assert_eq!(body["customers"][0]["counterparty"]["name"], "Haldenfjord Motors Corp");
        assert_eq!(body["customers"][0]["status"], "approved");
        assert_eq!(body["company"]["name"], "Nordvik Test Components Inc.");

        let (_, all) = call(&store, get_req("/companies/42/chain?status=all")).await;
        assert_eq!(all["suppliers"].as_array().unwrap().len(), 1);
        assert_eq!(call(&store, get_req("/companies/42/chain?status=maybe")).await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn analysis_lists_findings_and_what_was_not_checked() {
        let (store, _, _) = seeded();
        let (status, body) = call(&store, get_req("/companies/42/analysis")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["company"], "Nordvik Test Components Inc.");
        assert_eq!((body["customers"].as_u64(), body["suppliers"].as_u64(), body["pending"].as_u64()), (Some(1), Some(1), Some(1)));
        let findings = body["findings"].as_array().unwrap();
        assert!(findings.iter().any(|f| f["area"] == "Underlag" && f["level"] == "watch"));
        assert!(findings.iter().any(|f| f["area"] == "Sanktioner" && f["text"].as_str().unwrap().contains("inte hämtad")));
    }

    #[tokio::test]
    async fn review_moves_a_relation_out_of_the_queue() {
        let (store, _, pending_id) = seeded();
        let (_, queue) = call(&store, get_req("/review/pending")).await;
        assert_eq!(queue.as_array().unwrap().len(), 1);
        assert_eq!(queue[0]["id"], pending_id);

        let uri = format!("/relations/{pending_id}/review");
        let (status, body) =
            call(&store, post_json(&uri, json!({"status": "rejected", "note": "not the same company"}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "rejected");
        assert_eq!(body["note"], "not the same company");
        assert!(body["reviewed_at"].is_i64());

        let (_, queue) = call(&store, get_req("/review/pending")).await;
        assert!(queue.as_array().unwrap().is_empty());
        let (_, shown) = call(&store, get_req(&format!("/relations/{pending_id}"))).await;
        assert_eq!(shown["evidence"]["quote"], "Polarvik Castings Ltd in a made up sentence");
    }

    #[tokio::test]
    async fn bad_review_requests_are_refused() {
        let (store, id, _) = seeded();
        let uri = format!("/relations/{id}/review");
        let (status, _) = call(&store, post_json(&uri, json!({"status": "maybe"}))).await;
        assert!(status.is_client_error());
        let (status, _) = call(&store, post_json("/relations/999/review", json!({"status": "approved"}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(call(&store, get_req("/relations/999")).await.0, StatusCode::NOT_FOUND);
    }
}

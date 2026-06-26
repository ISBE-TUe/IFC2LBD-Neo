//! Integration tests for worker-rml-rust.
//!
//! These tests hit a live container over HTTP — they do NOT start the server
//! in-process. The goal is to exercise the exact binary that ships in Docker,
//! including multipart parsing, RML execution, tempfile handling, and the
//! serde shape of the JSON response.
//!
//! # Running
//!
//! Bring the container up first:
//!
//!     make test-up   # or:
//!     docker compose -f docker/docker-compose.test.yaml up -d --build worker-rml
//!
//! Then:
//!
//!     cd packages/worker-rml-rust
//!     cargo test --test integration
//!
//! Override the URL with WORKER_URL env var (defaults to http://localhost:18081).

use std::env;
use std::time::Duration;

use reqwest::{multipart, Client};

fn worker_url() -> String {
    env::var("WORKER_URL").unwrap_or_else(|_| "http://localhost:18081".to_string())
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("failed to build reqwest client")
}

async fn assert_worker_reachable(c: &Client) {
    let url = format!("{}/healthz", worker_url());
    let res = c
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("worker not reachable at {}: {}\nRun `make test-up` first.", url, e));
    assert!(
        res.status().is_success(),
        "worker /healthz returned {}",
        res.status()
    );
}

// ── /healthz ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn healthz_returns_ok_and_rust_native_flag() {
    let c = client();
    assert_worker_reachable(&c).await;

    let res = c
        .get(format!("{}/healthz", worker_url()))
        .send()
        .await
        .expect("healthz request failed");

    assert!(res.status().is_success());
    let body: serde_json::Value = res.json().await.expect("healthz body not JSON");

    assert_eq!(body["status"], "ok");
    assert_eq!(body["rust_native"], true);
    assert_eq!(body["java_available"], false);
    assert!(body["version"].is_string(), "version must be a string");
}

// ── /execute — happy path ────────────────────────────────────────────────

/// Minimal CSV + RML mapping fixture.
///
/// CSV: two rows of people with name + age.
/// RML: maps each row to ex:Person with schema:name and schema:age.
/// Expected output: 2 instances of ex:Person, each with name and age triples
/// (≈6 triples total depending on rdf:type materialization).
const CSV_FIXTURE: &str = "id,name,age\n1,Alice,30\n2,Bob,42\n";

const RML_FIXTURE: &str = r#"@prefix rr: <http://www.w3.org/ns/r2rml#> .
@prefix rml: <http://semweb.mmlab.be/ns/rml#> .
@prefix ql: <http://semweb.mmlab.be/ns/ql#> .
@prefix schema: <http://schema.org/> .
@prefix ex: <http://example.org/> .

<#TriplesMap>
  a rr:TriplesMap ;
  rml:logicalSource [
    rml:source "source.csv" ;
    rml:referenceFormulation ql:CSV
  ] ;
  rr:subjectMap [
    rr:template "http://example.org/person/{id}" ;
    rr:class ex:Person
  ] ;
  rr:predicateObjectMap [
    rr:predicate schema:name ;
    rr:objectMap [ rml:reference "name" ]
  ] ;
  rr:predicateObjectMap [
    rr:predicate schema:age ;
    rr:objectMap [ rml:reference "age" ; rr:datatype <http://www.w3.org/2001/XMLSchema#integer> ]
  ] .
"#;

#[tokio::test]
async fn execute_csv_to_turtle_produces_expected_triples() {
    let c = client();
    assert_worker_reachable(&c).await;

    let form = multipart::Form::new()
        .part(
            "file",
            multipart::Part::bytes(CSV_FIXTURE.as_bytes().to_vec())
                .file_name("source.csv")
                .mime_str("text/csv")
                .unwrap(),
        )
        .part(
            "mapping",
            multipart::Part::bytes(RML_FIXTURE.as_bytes().to_vec())
                .file_name("mapping.ttl")
                .mime_str("text/turtle")
                .unwrap(),
        )
        .text("output_format", "turtle");

    let res = c
        .post(format!("{}/execute", worker_url()))
        .multipart(form)
        .send()
        .await
        .expect("execute request failed");

    let status = res.status();
    let body_text = res.text().await.expect("execute body not readable");
    assert!(
        status.is_success(),
        "expected 2xx from /execute, got {}: {}",
        status,
        body_text
    );

    let body: serde_json::Value =
        serde_json::from_str(&body_text).expect("execute body not JSON");

    assert_eq!(body["format"], "turtle");
    assert!(
        body["triple_count_estimate"].as_u64().unwrap_or(0) >= 4,
        "expected >=4 triples, got {:?} — full body: {}",
        body["triple_count_estimate"],
        body_text
    );

    let rdf = body["rdf"]
        .as_str()
        .expect("rdf field must be a string");

    // Both people must appear in the output.
    assert!(
        rdf.contains("Alice") && rdf.contains("Bob"),
        "Both Alice and Bob must appear in output RDF. Got:\n{}",
        rdf
    );
    // Both Person subjects must appear.
    assert!(
        rdf.contains("person/1") && rdf.contains("person/2"),
        "Both person/1 and person/2 IRIs must appear. Got:\n{}",
        rdf
    );
    // Execution time must be reported.
    assert!(
        body["execution_time_ms"].as_u64().is_some(),
        "execution_time_ms must be reported"
    );
}

// ── /execute — error paths ──────────────────────────────────────────────

#[tokio::test]
async fn execute_missing_file_returns_4xx() {
    let c = client();
    assert_worker_reachable(&c).await;

    let form = multipart::Form::new().part(
        "mapping",
        multipart::Part::bytes(RML_FIXTURE.as_bytes().to_vec())
            .file_name("mapping.ttl")
            .mime_str("text/turtle")
            .unwrap(),
    );

    let res = c
        .post(format!("{}/execute", worker_url()))
        .multipart(form)
        .send()
        .await
        .expect("execute request failed");

    assert!(
        res.status().is_client_error(),
        "missing file should return 4xx, got {}",
        res.status()
    );
}

#[tokio::test]
async fn execute_missing_mapping_returns_4xx() {
    let c = client();
    assert_worker_reachable(&c).await;

    let form = multipart::Form::new().part(
        "file",
        multipart::Part::bytes(CSV_FIXTURE.as_bytes().to_vec())
            .file_name("source.csv")
            .mime_str("text/csv")
            .unwrap(),
    );

    let res = c
        .post(format!("{}/execute", worker_url()))
        .multipart(form)
        .send()
        .await
        .expect("execute request failed");

    assert!(
        res.status().is_client_error(),
        "missing mapping should return 4xx, got {}",
        res.status()
    );
}

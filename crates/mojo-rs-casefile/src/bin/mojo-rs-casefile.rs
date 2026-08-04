//! mojo-rs-casefile CLI: court-side tooling.
//!
//! Subcommands:
//!   validate <casefile>           — validate a casefile against its schema
//!   compare <casefile> <oracle> <candidate> — differential comparison

use std::path::Path;
use std::process::ExitCode;

use mojo_rs_casefile::casefile::Casefile;
use mojo_rs_casefile::compare::compare;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: mojo-rs-casefile <validate|compare> ...");
        return ExitCode::FAILURE;
    }
    match args[1].as_str() {
        "validate" => {
            if args.len() != 3 {
                eprintln!("usage: mojo-rs-casefile validate <casefile.json>");
                return ExitCode::FAILURE;
            }
            match validate_casefile(&args[2]) {
                Ok(()) => {
                    println!("casefile OK: {}", args[2]);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("casefile INVALID: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "compare" => {
            if args.len() != 5 {
                eprintln!(
                    "usage: mojo-rs-casefile compare <casefile.json> <oracle.events.jsonl> <candidate.events.jsonl>"
                );
                return ExitCode::FAILURE;
            }
            let (casefile, oracle_path, candidate_path) = (&args[2], &args[3], &args[4]);
            let result = run_compare(casefile, oracle_path, candidate_path);
            match result {
                Ok(json) => {
                    println!("{json}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("compare error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "--self-check" => {
            println!("mojo-rs-casefile self-check ok");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            ExitCode::FAILURE
        }
    }
}

/// Validate a casefile: parse + structural checks.
fn validate_casefile(path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let cf: Casefile = serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
    if cf.schema_version != 1 {
        return Err(format!("unsupported schema_version {}", cf.schema_version));
    }
    if cf.case_id.is_empty() {
        return Err("case_id is empty".to_string());
    }
    // Op ids must be unique and sequential.
    let mut ids: Vec<u64> = cf.operations.iter().map(|o| o.id).collect();
    ids.sort_unstable();
    for (i, id) in ids.iter().enumerate() {
        if *id != (i as u64 + 1) {
            return Err(format!(
                "operation ids not sequential: expected {}, got {}",
                i + 1,
                id
            ));
        }
    }
    // The casefile filename must match the case id.
    let file_stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if !file_stem.starts_with(&cf.case_id) {
        return Err(format!(
            "filename stem {file_stem} does not match case_id {}",
            cf.case_id
        ));
    }
    Ok(())
}

/// Run a comparison and emit the JSON result.
fn run_compare(
    casefile_path: &str,
    oracle_path: &str,
    candidate_path: &str,
) -> Result<String, String> {
    let cf_text =
        std::fs::read_to_string(casefile_path).map_err(|e| format!("read {casefile_path}: {e}"))?;
    let cf: Casefile =
        serde_json::from_str(&cf_text).map_err(|e| format!("parse casefile: {e}"))?;
    let oracle =
        std::fs::read_to_string(oracle_path).map_err(|e| format!("read {oracle_path}: {e}"))?;
    let candidate = std::fs::read_to_string(candidate_path)
        .map_err(|e| format!("read {candidate_path}: {e}"))?;
    let normalizer_ids: Vec<String> = cf.normalizers.iter().map(|n| n.id.clone()).collect();
    let cmp = compare(&cf.case_id, &oracle, &candidate, &normalizer_ids)?;
    serde_json::to_string_pretty(&cmp).map_err(|e| format!("serialize comparison: {e}"))
}

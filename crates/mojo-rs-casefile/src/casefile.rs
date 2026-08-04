//! Casefile model: the replayable court input shared by the oracle driver
//! (C++) and the candidate harness (Rust). Schema:
//! `casefiles/schema/casefile.schema.json`.

use serde::Deserialize;

/// A replayable case file.
#[derive(Debug, Clone, Deserialize)]
pub struct Casefile {
    /// Schema version (1).
    pub schema_version: u32,
    /// Unique case id (e.g. `MESSAGE_PIPE.BASIC.001`).
    pub case_id: String,
    /// The court this case belongs to.
    pub court: String,
    /// Pinned reference revision.
    pub reference_revision: String,
    /// Determinism seed.
    pub seed: u64,
    /// Preconditions (capabilities, init mode).
    pub preconditions: Preconditions,
    /// Parent-process operations.
    pub operations: Vec<Operation>,
    /// Optional named peer processes (multi-process cases).
    #[serde(default)]
    pub processes: std::collections::BTreeMap<String, Process>,
    /// Named contract assertions.
    pub expected_contract: serde_json::Value,
    /// Normalizer ids applied before comparison.
    pub normalizers: Vec<NormalizerRef>,
}

/// Preconditions for running a case.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Preconditions {
    /// Capabilities the harness must support.
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// Initialization options.
    #[serde(default)]
    pub init: Option<InitOptions>,
}

/// Initialization options.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitOptions {
    /// `default`, `broker`, or `non-broker`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Raw flags.
    #[serde(default)]
    pub flags: Option<u32>,
}

/// A peer process definition.
#[derive(Debug, Clone, Deserialize)]
pub struct Process {
    /// Operations executed in the peer process.
    pub operations: Vec<Operation>,
    /// The parent process to bootstrap from (default: parent).
    #[serde(default)]
    pub bootstrap: Option<String>,
}

/// A single operation.
#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    /// Sequential id.
    pub id: u64,
    /// Operation name (see the schema's enum).
    pub op: String,
    /// Operation arguments.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Expected outcomes.
    #[serde(default)]
    pub expect: Option<Expectation>,
    /// Human note.
    #[serde(default)]
    pub note: Option<String>,
}

/// Expected outcomes for an operation.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Expectation {
    /// Expected `MOJO_RESULT_*` name.
    #[serde(default)]
    pub result: Option<String>,
    /// Handle tokens this operation produces.
    #[serde(default)]
    pub produce: Vec<String>,
    /// Event kinds expected.
    #[serde(default)]
    pub events: Vec<String>,
}

/// A normalizer reference.
#[derive(Debug, Clone, Deserialize)]
pub struct NormalizerRef {
    /// Registry id of the normalizer.
    pub id: String,
    /// Normalizer parameters.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// The name of the parent process in multi-process cases.
pub const PARENT_PROCESS: &str = "parent";

impl Casefile {
    /// The name of a process in this case (parent or a named peer).
    pub fn process_name(&self) -> &'static str {
        PARENT_PROCESS
    }
}

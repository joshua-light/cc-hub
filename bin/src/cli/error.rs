use cc_hub_lib::ops::OpError;

use super::print_json;

/// Terminate a verb by emitting the one-JSON-line contract and returning the
/// process exit code.
///
/// The module contract is "one JSON line on stdout". On success the verb
/// already printed its own JSON; on error we print a structured error line
/// here — `{"ok":false,"error":"<msg>","kind":"<usage|notfound|conflict|
/// other>"}` (plus `recipe` when the variant carries a hint) — so a caller
/// piping stdout to `jq` always sees a parseable result, even
/// on the failures that matter most. A short human line still goes to
/// stderr for interactive use.
pub(super) fn handle(result: Result<(), CliError>) -> i32 {
    match result {
        Ok(()) => 0,
        // The verb already printed a richer `{"ok":false,...}` line carrying
        // domain fields the generic shape can't express. Don't double-print
        // JSON — just emit a human line and the exit code.
        Err(CliError::Reported(msg)) => {
            eprintln!("error: {}", msg);
            1
        }
        Err(err) => {
            let kind = err.kind();
            let (msg, recipe) = err.into_message_and_recipe();
            let mut payload = serde_json::json!({
                "ok": false,
                "error": msg,
                "kind": kind,
            });
            if let Some(recipe) = recipe.as_deref() {
                payload["recipe"] = serde_json::Value::String(recipe.to_string());
            }
            print_json(&payload);
            eprintln!("{} error: {}", kind, msg);
            match kind {
                "usage" => 2,
                _ => 1,
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum CliError {
    /// Bad invocation: missing/unknown flag, malformed value, illegal
    /// transition the caller could have avoided. Exit 2, kind "usage".
    Usage(String),
    /// Requested entity does not exist. Exit 1, kind "notfound".
    NotFound(String),
    /// State guard tripped. Exit 1, kind "conflict". Carries an optional
    /// recipe the caller can act on.
    Conflict { msg: String, recipe: Option<String> },
    /// Everything else (I/O, serialization). Exit 1,
    /// kind "other".
    Other(String),
    /// The verb already printed its own `{"ok":false,...}` JSON line (a rich,
    /// domain-specific payload). `handle` must NOT print a second JSON line;
    /// it only sets the nonzero exit code and a human stderr line. The string
    /// is that stderr message.
    Reported(String),
}

impl CliError {
    /// Stable machine-readable category for the JSON error contract.
    fn kind(&self) -> &'static str {
        match self {
            CliError::Usage(_) => "usage",
            CliError::NotFound(_) => "notfound",
            CliError::Conflict { .. } => "conflict",
            CliError::Other(_) => "other",
            // Never surfaced as a `kind` (handled before this is consulted),
            // but map it for completeness.
            CliError::Reported(_) => "other",
        }
    }

    /// Decompose into the human message and an optional remediation recipe.
    fn into_message_and_recipe(self) -> (String, Option<String>) {
        match self {
            CliError::Usage(msg)
            | CliError::NotFound(msg)
            | CliError::Other(msg)
            | CliError::Reported(msg) => (msg, None),
            CliError::Conflict { msg, recipe } => (msg, recipe),
        }
    }
}

impl From<OpError> for CliError {
    /// Lossless 1:1 mapping from the domain-layer error to the CLI error so
    /// the JSON error contract (kind / recipe / exit code) is unchanged.
    fn from(e: OpError) -> Self {
        match e {
            OpError::Usage(msg) => CliError::Usage(msg),
            OpError::NotFound(msg) => CliError::NotFound(msg),
            OpError::Conflict { msg, recipe } => CliError::Conflict { msg, recipe },
            OpError::Other(msg) => CliError::Other(msg),
            OpError::Reported(msg) => CliError::Reported(msg),
        }
    }
}

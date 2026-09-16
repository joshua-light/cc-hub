//! `cc-hub wake <name>` — say a named thing happened, so every agent whose
//! spec lists it polls now instead of at the end of its interval. The board
//! wakes `board` itself on every card status change; this verb is here so a
//! script that changes something the hub cannot see can do the same.

use super::{print_json, CliError};
use cc_hub_lib::wake::Wake;

pub(crate) fn wake(args: &[String]) -> Result<(), CliError> {
    let name = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| CliError::Usage("wake <name> (e.g. `cc-hub wake board`)".into()))?;
    let wake = Wake::named(name)
        .ok_or_else(|| CliError::Usage(format!("not a usable wake name: {:?}", name)))?;
    wake.now().map_err(|e| CliError::Other(e.to_string()))?;
    print_json(&serde_json::json!({ "ok": true, "wake": wake.name() }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_name_is_required() {
        let err = wake(&s(&["--json"])).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn a_name_that_cannot_be_a_file_is_refused() {
        let err = wake(&s(&["../escape"])).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }
}

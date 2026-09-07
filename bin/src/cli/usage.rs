//! `cc-hub usage`: the shared quota store, for scripts.
use super::CliError;
use cc_hub_lib::platform::paths;
use cc_hub_lib::usage::{self, Account};

pub(crate) fn usage(args: &[String]) -> Result<(), CliError> {
    let mut home = None;
    let mut force = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--home" => {
                home = Some(
                    it.next()
                        .ok_or_else(|| CliError::Usage("--home requires a directory".into()))?,
                )
            }
            "--refresh" => force = true,
            other => return Err(CliError::Usage(format!("unknown argument: {other}"))),
        }
    }
    let account = match home {
        Some(dir) => Account::at(paths::expand_home(dir)),
        None => Account::current()
            .ok_or_else(|| CliError::Other("cannot resolve the Claude home".into()))?,
    };
    let usage = if force {
        usage::refresh(&account)
    } else {
        usage::read(&account)
    };
    super::print_json(&usage.report(&account));
    Ok(())
}

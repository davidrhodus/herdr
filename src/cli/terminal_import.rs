use crate::api::schema::{EmptyParams, Method, Request, TerminalSessionImportParams};
use std::io;

pub(super) fn run(args: &[String]) -> io::Result<i32> {
    match args {
        [action] if action == "list" => super::print_response(&super::send_request(&Request {
            id: "cli:terminal:sessions".into(),
            method: Method::TerminalSessionsList(EmptyParams::default()),
        })?),
        [action, token] if action == "start" => {
            super::print_response(&super::send_request(&Request {
                id: "cli:terminal:import".into(),
                method: Method::TerminalSessionImport(TerminalSessionImportParams {
                    source: crate::terminal_import::decode(token)?,
                }),
            })?)
        }
        [action] if action == "install-helper" => {
            crate::platform::terminal_import::install_helper()?;
            Ok(0)
        }
        [action, token] if action == "run" || action == "privileged-run" => {
            crate::platform::terminal_import::run(
                &crate::terminal_import::decode(token)?,
                action == "privileged-run",
            )?;
            Ok(0)
        }
        _ => {
            eprintln!("usage: herdr terminal-import list | start <identity> | install-helper");
            Ok(2)
        }
    }
}

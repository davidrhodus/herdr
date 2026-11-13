//! Guided enrollment uses no provider-wide secret in its default browser mode.
use super::{input, model::*, safe, transport::*};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, io};
#[cfg(test)]
thread_local! {pub(super) static TEST_STATUS:std::cell::RefCell<Option<Value>>=const {std::cell::RefCell::new(None)};}

pub(super) fn identity(status: &Value) -> io::Result<String> {
    if status["BackendState"] != "Running" {
        return Err(error(
            "Connect this computer in Tailscale before creating a cloud server.",
        ));
    }
    let current = &status["CurrentTailnet"];
    let (kind, name) = current["MagicDNSSuffix"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(|n| ("dns", n))
        .or_else(|| {
            current["Name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(|n| ("name", n))
        })
        .ok_or_else(|| {
            error("Cannot identify the current Tailscale network. Update or reconnect Tailscale.")
        })?;
    if name.len() > 253 || name.chars().any(char::is_control) {
        return Err(error("Invalid Tailscale network identity"));
    }
    Ok(format!("{kind}:{name}"))
}
pub(super) fn status() -> io::Result<Value> {
    #[cfg(test)]
    if let Some(status) = TEST_STATUS.with(|v| v.borrow().clone()) {
        return Ok(status);
    }
    json(&crate::client::tailscale::status_bytes().map_err(error)?)
}
pub(super) fn ensure() -> io::Result<String> {
    loop {
        match status().and_then(|s| identity(&s)) {
            Ok(id) => {
                println!(
                    "Tailscale network: {}. Approve new servers into this same network.",
                    safe(&id)
                );
                return Ok(id);
            }
            Err(e) => {
                println!(
                    "{} Open Tailscale and sign in or connect.",
                    safe(&e.to_string())
                );
                if input("Press Return to retry, or type 0 to cancel", "")? == "0" {
                    return Err(error("Setup cancelled before instance creation"));
                }
            }
        }
    }
}
pub(super) fn check(job: &Job, status: &Value) -> io::Result<()> {
    if let Some(expected) = &job.expected_tailnet {
        if &identity(status)? != expected {
            return Err(error("This manager changed Tailscale networks. Reconnect to the original network before resuming this job."));
        }
    }
    Ok(())
}
fn marker<'a>(console: &'a str, kind: &str, id: &str) -> Option<&'a str> {
    let prefix = format!("{kind} {id} ");
    console
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(&prefix))
}
pub(super) fn current_console<'a>(job: &Job, console: &'a str, id: &str) -> Option<&'a str> {
    if let Some(nonce) = &job.reauth_nonce {
        let line = format!("HERDR-ENROLLMENT-START {id} {nonce}");
        let start = console.lines().find(|candidate| *candidate == line)?;
        let offset = console.find(start)?;
        return Some(&console[offset + start.len()..]);
    }
    Some(console)
}
pub(super) fn login_url(console: &str, id: &str) -> Option<String> {
    let value = marker(console, "HERDR-LOGIN", id)?;
    let token = value.strip_prefix("https://login.tailscale.com/a/")?;
    (token.len() >= 8
        && token.len() <= 256
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)))
    .then(|| value.to_owned())
}
pub(super) fn check_remote(job: &Job, console: &str, id: &str) -> io::Result<()> {
    let Some(expected) = &job.expected_tailnet else {
        return Ok(());
    };
    if let Some(value) = marker(console, "HERDR-TAILNET", id) {
        if value.len() > 2048 {
            return Err(error("Invalid server network report"));
        }
        let current: Value =
            serde_json::from_str(value).map_err(|_| error("Invalid server network report"))?;
        let status = serde_json::json!({"BackendState":"Running","CurrentTailnet":current});
        if identity(&status)? != *expected {
            return Err(error("The server was approved into a different Tailscale network. Use Manage → repair-enrollment, then approve it into this manager's network."));
        }
    }
    Ok(())
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserReceipt {
    seen: BTreeMap<String, Vec<String>>,
}
/// Store hashes, not login capabilities. Repeated polls/resumes never reopen a
/// previously opened link, and rotating links cannot cause unlimited tabs.
pub(super) fn should_open(job: &Job, id: &str, url: &str) -> io::Result<bool> {
    use sha2::{Digest, Sha256};
    let path = job_dir(&job.id)?.join("browser-receipt.json");
    let mut receipt: BrowserReceipt = if path.exists() {
        read(&path)?
    } else {
        Default::default()
    };
    let hashes = receipt.seen.entry(id.into()).or_default();
    let hash = format!("{:x}", Sha256::digest(url.as_bytes()));
    if hashes.contains(&hash) || hashes.len() >= 3 {
        return Ok(false);
    }
    hashes.push(hash);
    save(&path, &receipt)?;
    Ok(true)
}
pub(super) fn reset_browser(job: &Job) -> io::Result<()> {
    save(
        &job_dir(&job.id)?.join("browser-receipt.json"),
        &BrowserReceipt::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_latest_job_scoped_canonical_tailscale_urls_are_accepted() {
        let good = "https://login.tailscale.com/a/1234abcd";
        assert_eq!(
            login_url(&format!("HERDR-LOGIN job {good}"), "job").as_deref(),
            Some(good)
        );
        assert!(login_url(&format!("HERDR-LOGIN other {good}"), "job").is_none());
        for bad in [
            "http://login.tailscale.com/a/1234abcd",
            "https://login.tailscale.com.evil/a/1234abcd",
            "https://user@login.tailscale.com/a/1234abcd",
            "https://login.tailscale.com:443/a/1234abcd",
            "https://login.tailscale.com/a/1234abcd?next=evil",
            "https://login.tailscale.com/a/1234abcd#bad",
            "https://login.tailscale.com/a/abc%0aevil",
        ] {
            assert!(login_url(
                &format!("HERDR-LOGIN job {good}\nHERDR-LOGIN job {bad}"),
                "job"
            )
            .is_none());
        }
    }
    #[test]
    fn local_and_remote_tailnet_switches_are_rejected() {
        let mut job = crate::cli::cloud::tests::job();
        job.expected_tailnet = Some("dns:tail1.ts.net".into());
        assert!(check(
            &job,
            &json!({"BackendState":"Running","CurrentTailnet":{"MagicDNSSuffix":"tail2.ts.net"}})
        )
        .is_err());
        assert!(check_remote(
            &job,
            "HERDR-TAILNET job {\"MagicDNSSuffix\":\"tail2.ts.net\"}",
            "job"
        )
        .is_err());
        assert!(check_remote(
            &job,
            "HERDR-TAILNET job {\"MagicDNSSuffix\":\"tail1.ts.net\"}",
            "job"
        )
        .is_ok());
    }
    #[test]
    fn login_receipts_deduplicate_across_polls_and_resume_without_persisting_urls() {
        let _context = crate::cli::cloud::tests::Context::new();
        let job = crate::cli::cloud::tests::job();
        let url = "https://login.tailscale.com/a/1234abcd";
        assert!(should_open(&job, &job.id, url).unwrap());
        assert!(!should_open(&job, &job.id, url).unwrap());
        let bytes = std::fs::read_to_string(job_dir(&job.id).unwrap().join("browser-receipt.json"))
            .unwrap();
        assert!(!bytes.contains("1234abcd"));
        for n in 0..2 {
            assert!(should_open(&job, &job.id, &format!("{url}{n}")).unwrap());
        }
        assert!(!should_open(&job, &job.id, &format!("{url}more")).unwrap());
    }
    #[test]
    fn reauth_ignores_old_console_proof_and_waits_for_this_attempt() {
        let mut job = crate::cli::cloud::tests::job();
        job.reauth_nonce = Some("new-attempt".into());
        let old="HERDR-READY job old-node 100.64.1.2 ssh-ed25519 old-key\nHERDR-ENROLLMENT-START job old-attempt\n";
        assert!(current_console(&job, old, "job").is_none());
        let console=format!("{old}HERDR-ENROLLMENT-START job new-attempt\nHERDR-LOGIN job https://login.tailscale.com/a/1234abcd\n");
        let scoped = current_console(&job, &console, "job").unwrap();
        assert!(!scoped.contains("old-node"));
        assert!(scoped.contains("HERDR-LOGIN"));
    }
    #[test]
    fn existing_oauth_settings_and_jobs_remain_readable() {
        let settings: Settings = serde_json::from_value(
            json!({"accounts":[],"oauth_client":"existing","tailnet":"-","tag":"tag:herdr"}),
        )
        .unwrap();
        assert_eq!(settings.enrollment_mode(), Enrollment::OAuth);
        let mut settings = settings;
        settings.enrollment = Some(Enrollment::Browser);
        assert_eq!(settings.enrollment_mode(), Enrollment::Browser);
        let job = crate::cli::cloud::tests::job();
        assert!(job.expected_tailnet.is_none());
        assert!(job.reauth_nonce.is_none());
    }
}

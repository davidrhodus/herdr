//! Bounded subprocess transport. Secrets use stdin or private files, never argv.
use super::model::{error, Account, Provider};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[cfg(test)]
type Fake = Box<dyn FnMut(&Command, &[u8]) -> io::Result<Vec<u8>>>;
#[cfg(test)]
thread_local! {pub(super) static FAKE: std::cell::RefCell<Option<Fake>>=const {std::cell::RefCell::new(None)};}

pub(super) fn capture(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    #[cfg(test)]
    if let Some(result) = FAKE.with(|fake| fake.borrow_mut().as_mut().map(|f| f(&command, input))) {
        return result;
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child=command.spawn().map_err(|e| error(format!("Cannot start provider tool: {e}. Configure its installation/account in Cloud accounts.")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| error("Missing input pipe"))?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    fn drain(mut stream: impl Read) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        (&mut stream)
            .take(8 * 1024 * 1024)
            .read_to_end(&mut bytes)?;
        io::copy(&mut stream, &mut io::sink())?;
        Ok(bytes)
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("Missing output pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| error("Missing error pipe"))?;
    let out = std::thread::spawn(move || drain(stdout));
    let err = std::thread::spawn(move || drain(stderr));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = writer.join();
    let bytes = out.join().map_err(|_| error("Output reader stopped"))??;
    let _ = err.join();
    if timed_out {
        return Err(error(
            "Provider request timed out; use Resume to reconcile the existing job",
        ));
    }
    if !status.success() {
        return Err(error(format!("Provider tool returned {status}. Check the selected account, permissions, location, and resource settings. The existing job is retained for Resume.")));
    }
    Ok(bytes)
}
pub(super) fn aws(account: &Account, args: &[&str]) -> io::Result<Value> {
    let mut c = Command::new(crate::platform::cloud_aws_program());
    c.args([
        "--profile",
        &account.configuration,
        "--region",
        &account.location,
        "--output",
        "json",
        "--no-cli-pager",
        "--cli-connect-timeout",
        "10",
        "--cli-read-timeout",
        "60",
    ])
    .args(args)
    .env("AWS_PAGER", "")
    .env("AWS_IGNORE_CONFIGURED_ENDPOINT_URLS", "true");
    let bytes = capture(c, &[], Duration::from_secs(90))?;
    if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        json(&bytes)
    }
}
pub(super) fn google(account: &Account, args: &[&str]) -> io::Result<Vec<u8>> {
    let mut c = Command::new("gcloud");
    c.args([
        "--configuration",
        &account.configuration,
        "--account",
        &account.principal,
        "--project",
        &account.project,
        "--quiet",
    ])
    .args(args);
    capture(c, &[], Duration::from_secs(90))
}
pub(super) fn json(bytes: &[u8]) -> io::Result<Value> {
    serde_json::from_slice(bytes).map_err(|_| error("Provider returned invalid JSON"))
}
pub(super) fn field<'a>(v: &'a Value, key: &str) -> io::Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error(format!("Provider response is missing {key}")))
}
pub(super) fn verify_account(account: &Account) -> io::Result<()> {
    let actual = match account.provider {
        Provider::Aws => {
            field(&aws(account, &["sts", "get-caller-identity"])?, "Account")?.to_owned()
        }
        Provider::Gcp => String::from_utf8(google(account, &["config", "get-value", "account"])?)
            .map_err(|_| error("Invalid Google account"))?
            .trim()
            .into(),
    };
    if actual != account.principal {
        return Err(error("The configured cloud account changed. Restore the original account before resuming this job."));
    }
    Ok(())
}
/// curl config syntax is JSON-compatible for these quoted scalar values.
fn quote(value: &str) -> io::Result<String> {
    if value.chars().any(char::is_control) {
        return Err(error("Invalid HTTP configuration"));
    }
    serde_json::to_string(value).map_err(|_| error("Cannot encode HTTP configuration"))
}
pub(super) fn http(
    method: &str,
    url: &str,
    bearer: Option<&str>,
    body: Option<&Value>,
    form: Option<&str>,
) -> io::Result<Value> {
    if !url.starts_with("https://") {
        return Err(error("Cloud requests require HTTPS"));
    }
    let mut config = format!(
        "url = {}\nrequest = {}\nsilent\nshow-error\nmax-time = 90\nconnect-timeout = 10\n",
        quote(url)?,
        quote(method)?
    );
    if let Some(bearer) = bearer {
        config += &format!(
            "header = {}\n",
            quote(&format!("Authorization: Bearer {bearer}"))?
        );
    }
    if let Some(body) = body {
        config += "header = \"Content-Type: application/json\"\n";
        config += &format!(
            "data = {}\n",
            quote(&serde_json::to_string(body).map_err(|_| error("Cannot encode cloud request"))?)?
        );
    }
    if let Some(form) = form {
        config += &format!("data = {}\n", quote(form)?);
    }
    let mut c = Command::new("curl");
    c.args([
        "--disable",
        "--config",
        "-",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--write-out",
        "\n%{http_code}",
    ]);
    let bytes = capture(c, config.as_bytes(), Duration::from_secs(100))?;
    let split = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .ok_or_else(|| error("Missing HTTP status"))?;
    let status =
        std::str::from_utf8(&bytes[split + 1..]).map_err(|_| error("Invalid HTTP status"))?;
    if status == "404" {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Cloud resource not found",
        ));
    }
    if !status.starts_with('2') {
        return Err(error(format!(
            "Cloud API returned HTTP {status}. Check account permissions and provider settings."
        )));
    }
    if split == 0 {
        Ok(Value::Null)
    } else {
        json(&bytes[..split])
    }
}
pub(super) fn gcp(
    account: &Account,
    method: &str,
    suffix: &str,
    body: Option<&Value>,
) -> io::Result<Value> {
    let token = String::from_utf8(google(account, &["auth", "print-access-token"])?)
        .map_err(|_| error("Invalid Google token"))?;
    let url = format!(
        "https://compute.googleapis.com/compute/v1/projects/{}/zones/{}/{}",
        account.project, account.location, suffix
    );
    http(method, &url, Some(token.trim()), body, None)
}
pub(super) fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_config_quoted_and_cannot_inject_headers() {
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        assert!(quote("token\nurl=bad").is_err());
        assert_eq!(encode("tag:herdr &+"), "tag%3Aherdr%20%26%2B");
    }
}

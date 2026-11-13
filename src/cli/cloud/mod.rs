//! Interactive client-local cloud control, hosted inside the machine manager PTY.
//! Provisioning authority is deliberately separate from catalog-sync authority.
mod aws_network;
mod bootstrap;
mod model;
mod nitro;
mod provider;
#[cfg(test)]
mod tests;
mod transport;
use model::*;
use std::{
    io::{self, IsTerminal, Write},
    process::Command,
};
use transport::*;

fn input(label: &str, default: &str) -> io::Result<String> {
    print!("{label} [{}]: ", safe(default));
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    if value.chars().any(char::is_control) || value.len() > 4096 {
        return Err(error(
            "Input exceeds the limit or contains control characters",
        ));
    }
    Ok(if value.is_empty() {
        default.into()
    } else {
        value.into()
    })
}
fn secret(label: &str) -> io::Result<String> {
    use crossterm::event::{read, Event, KeyCode, KeyEventKind};
    print!("{label}: ");
    io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    let _restore = Restore;
    let mut value = String::new();
    loop {
        if let Event::Key(k) = read()? {
            if k.kind == KeyEventKind::Release {
                continue;
            }
            match k.code {
                KeyCode::Enter => break,
                KeyCode::Esc => return Err(error("Cancelled")),
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) if !c.is_control() && value.len() < 4096 => value.push(c),
                _ => {}
            }
        }
    }
    print!("\r\n");
    if value.is_empty() {
        return Err(error("Secret cannot be empty"));
    }
    Ok(value)
}
fn safe(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect()
}
fn confirm(text: &str, word: &str) -> io::Result<bool> {
    println!("{text}");
    Ok(input(&format!("Type {word} to continue"), "")? == word)
}
pub(super) fn run(args: &[String]) -> io::Result<i32> {
    if args != ["console"] || !io::stdin().is_terminal() {
        return Err(error("Open Servers → Cloud instances in the TUI"));
    }
    // A kernel-held lease survives file reuse and releases on crash/cancellation.
    let path = root()?.join("manager.lock");
    let lock = match crate::platform::create_private_state_file(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            crate::platform::validate_private_key_path(&path, false)?;
            std::fs::OpenOptions::new().write(true).open(path)?
        }
        Err(e) => return Err(e),
    };
    lock.try_lock()
        .map_err(|_| error("Cloud setup is open in another local Herdr window"))?;
    loop {
        println!("\nCloud instances\n  1  Create AWS EC2 instance\n  2  Create AWS Nitro Sandbox\n  3  Create Google Cloud instance\n  4  Manage / resume existing jobs\n  5  Configure AWS account and region\n  6  Configure Google account, project and zone\n  7  Configure Tailscale enrollment\n  8  Choose / create AWS networking\n  9  Resume network creation\n  0  Back\nEnter a number, then Return. Ctrl+G closes this panel; created resources remain in Manage.");
        let result = match input("Choose", "")?.as_str() {
            "0" => return Ok(0),
            "1" => create(Provider::Aws, false),
            "2" => create(Provider::Aws, true),
            "3" => create(Provider::Gcp, false),
            "4" => manage(),
            "5" => configure(Provider::Aws),
            "6" => configure(Provider::Gcp),
            "7" => tailscale(),
            "8" => aws_network::configure(),
            "9" => aws_network::resume(),
            _ => continue,
        };
        if let Err(e) = result {
            println!("\n{}\n", safe(&e.to_string()));
        }
    }
}
fn configure(provider: Provider) -> io::Result<()> {
    println!("\nAccount settings stay on this computer. Sign-in uses the official AWS/gcloud tool; secrets are never added to the shared server list.");
    let configuration = input("Local account/profile name", "default")?;
    token(&configuration)?;
    let login = input("Open provider sign-in here? (yes/no)", "no")?;
    if login == "yes" {
        let mut c = match provider {
            Provider::Aws => {
                let mut c = Command::new("aws");
                c.args(["configure", "sso", "--profile", &configuration]);
                c
            }
            Provider::Gcp => {
                let mut describe = Command::new("gcloud");
                describe.args([
                    "config",
                    "configurations",
                    "describe",
                    &configuration,
                    "--format=json",
                ]);
                if capture(describe, &[], std::time::Duration::from_secs(30)).is_err() {
                    let mut create = Command::new("gcloud");
                    create.args([
                        "config",
                        "configurations",
                        "create",
                        &configuration,
                        "--no-activate",
                    ]);
                    if !create.status()?.success() {
                        return Err(error(
                            "Could not create the named Google Cloud configuration",
                        ));
                    }
                }
                let mut c = Command::new("gcloud");
                c.args(["auth", "login", "--configuration", &configuration]);
                c
            }
        };
        if !c.status()?.success() {
            return Err(error("Provider sign-in did not finish"));
        }
    }
    let mut a = Account {
        name: configuration.clone(),
        provider: provider.clone(),
        configuration,
        principal: String::new(),
        location: String::new(),
        project: String::new(),
        image: String::new(),
        size: String::new(),
        subnet: String::new(),
        security_group: String::new(),
    };
    match provider {
        Provider::Aws => {
            a.location = input("AWS region", "us-west-2")?;
            token(&a.location)?;
            a.principal = field(&aws(&a, &["sts", "get-caller-identity"])?, "Account")?.into();
            println!("Verified AWS account {}", safe(&a.principal));
            a.image = input(
                "AMI ID or SSM public AMI parameter (Linux with systemd)",
                "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-6.18-x86_64",
            )?;
            a.size = input("Instance type", "t3.small")?;
            aws_network::select(&mut a)?;
        }
        Provider::Gcp => {
            a.principal = input("Google account email", "")?;
            a.project = input("Project ID", "")?;
            a.location = input("Zone", "us-central1-a")?;
            a.image = input(
                "Linux image (full projects/.../global/images/... path)",
                "projects/debian-cloud/global/images/family/debian-12",
            )?;
            a.size = input("Machine type", "e2-small")?;
            a.subnet = input("Subnetwork name", "default")?;
            verify_account(&a)?;
        }
    }
    for v in [&a.principal, &a.location, &a.image, &a.size, &a.subnet] {
        token(v)?;
    }
    let mut settings = settings()?;
    settings
        .accounts
        .retain(|old| !(old.provider == a.provider && old.configuration == a.configuration));
    settings.accounts.push(a);
    save(&root()?.join("accounts.json"), &settings)?;
    println!("Account saved. Choose Create to review an instance.");
    Ok(())
}
fn tailscale() -> io::Result<()> {
    println!("Create a Tailscale OAuth client with auth_keys write scope and a server tag. The OAuth secret stays in a private local file; VMs get single-use 24-hour keys. Tailnet policy must permit your managers to reach the tagged servers on SSH.");
    let mut s = settings()?;
    s.oauth_client = input("OAuth client ID", &s.oauth_client)?;
    s.tailnet = input(
        "Tailnet name (or - for the authenticated tailnet)",
        if s.tailnet.is_empty() {
            "-"
        } else {
            &s.tailnet
        },
    )?;
    s.tag = input(
        "Server tag",
        if s.tag.is_empty() {
            "tag:herdr"
        } else {
            &s.tag
        },
    )?;
    token(&s.oauth_client)?;
    if s.tailnet != "-" {
        token(&s.tailnet)?;
    }
    token(&s.tag)?;
    if !s.tag.starts_with("tag:") {
        return Err(error("Server tag must start with tag:"));
    }
    let value = secret("OAuth secret (hidden)")?;
    write(&root()?.join("tailscale-oauth-secret"), value.as_bytes())?;
    save(&root()?.join("accounts.json"), &s)?;
    println!("Tailscale enrollment configured.");
    Ok(())
}
fn choose_account(provider: &Provider) -> io::Result<Account> {
    let s = settings()?;
    let accounts: Vec<_> = s
        .accounts
        .iter()
        .filter(|a| &a.provider == provider)
        .collect();
    if accounts.is_empty() {
        return Err(error(
            "Configure this provider account first (Cloud instances → Configure account)",
        ));
    }
    for (i, a) in accounts.iter().enumerate() {
        println!(
            "{}  {} · {} · {}",
            i + 1,
            safe(&a.name),
            safe(&a.principal),
            safe(&a.location)
        );
    }
    let n: usize = input("Account", "1")?
        .parse()
        .map_err(|_| error("Choose an account number"))?;
    accounts
        .get(n.wrapping_sub(1))
        .map(|a| (*a).clone())
        .ok_or_else(|| error("No such account"))
}
fn create(provider: Provider, nitro: bool) -> io::Result<()> {
    let mut account = choose_account(&provider)?;
    let name = input("Server name", "herdr-server")?;
    token(&name)?;
    account.size = input(
        "Instance size",
        if nitro { "m7i.4xlarge" } else { &account.size },
    )?;
    token(&account.size)?;
    if nitro && !account.image.contains("kernel-6.18") {
        return Err(error("Nitro Sandboxes require an AL2023 kernel 6.18 AMI. Configure the matching AWS image first."));
    }
    verify_account(&account)?;
    if provider == Provider::Aws
        && input("Use saved networking? (yes / choose)", "yes")? == "choose"
    {
        aws_network::select(&mut account)?;
    }
    println!("\nCreate review\n  Account: {}\n  Location: {}\n  Size: {}\n  Image: {}\n  Network: {}\n  Outbound internet: external address\n  Public SSH: blocked\n  SSH user: herdr · dedicated local key\n  Tailscale: single-use enrollment → verified SSH → current Rust build",safe(&account.principal),safe(&account.location),safe(&account.size),safe(&account.image),safe(&account.subnet));
    if nitro {
        println!("  Nitro: one dedicated EC2 parent and one direct-mode guest; persistent 16 GiB guest root disk. Parent preparation reboots this new dedicated parent only.");
    }
    if !confirm("Provider charges apply while resources exist. Stop retains disks and their charges; Delete terminates this job's instance.","CREATE")? {return Ok(());}
    let id = uuid()?;
    let public = bootstrap::identity(&id)?;
    let auth = bootstrap::enrollment(&settings()?)?;
    let host = format!("herdr-{}", &id[..18]);
    let script = bootstrap::script(&id, &public, &auth, &host)?;
    let request = provider::create_request(&account, &id, &name, &script, &public, nitro)?;
    let mut job = Job {
        id,
        account,
        name,
        nitro,
        request,
        resource: None,
        operation: None,
        submitted: false,
        first_submission: None,
        phase: "Reviewed".into(),
        target: None,
        endpoint: None,
        deleted: false,
        pending_action: None,
    };
    job.save()?;
    println!(
        "Saved job {}. Resume remains available after closing this panel.",
        job.id
    );
    resume(&mut job)
}
fn resume(job: &mut Job) -> io::Result<()> {
    if matches!(
        job.phase.as_str(),
        "start failed" | "stop failed" | "delete failed"
    ) {
        return Err(error("The lifecycle action failed. Choose Start, Stop, or Delete explicitly to retry after checking provider permissions."));
    }
    if provider::finish_pending(job)? {
        return Ok(());
    }
    provider::create(job)?;
    let proof = bootstrap::wait_proof(job)?;
    if job.nitro {
        nitro::prepare(job, proof)?;
    } else {
        bootstrap::connect(job, proof)?;
    }
    Ok(())
}
fn manage() -> io::Result<()> {
    let mut jobs = jobs()?;
    if jobs.is_empty() {
        println!("No cloud jobs on this computer yet.");
        return Ok(());
    }
    for (i, j) in jobs.iter().enumerate() {
        println!(
            "{}  {} · {} · {} · {}",
            i + 1,
            safe(&j.name),
            safe(&j.account.location),
            safe(&j.phase),
            safe(j.resource.as_deref().unwrap_or("pending"))
        );
    }
    let n: usize = input("Job number (0 goes back)", "0")?
        .parse()
        .map_err(|_| error("Choose a job number"))?;
    if n == 0 {
        return Ok(());
    }
    let j = jobs.get_mut(n - 1).ok_or_else(|| error("No such job"))?;
    match input(
        "Action: resume / repair-enrollment / start / stop / delete / back",
        "back",
    )?
    .as_str()
    {
        "resume" => resume(j),
        "repair-enrollment" => {
            if j.endpoint.is_some() {
                return Err(error(
                    "This server is already ready. Use the saved server's SSH repair instead.",
                ));
            }
            if !confirm("Repair first-boot enrollment? EC2 parents will stop and restart. This applies only to a job that has never become Ready.","REPAIR")? {return Ok(());}
            let public = bootstrap::identity(&j.id)?;
            let auth = bootstrap::enrollment(&settings()?)?;
            let script =
                bootstrap::script(&j.id, &public, &auth, &format!("herdr-{}", &j.id[..18]))?;
            provider::repair_enrollment(j, &script)?;
            if j.nitro {
                let child_auth = bootstrap::enrollment(&settings()?)?;
                let child = bootstrap::script(
                    &format!("{}-guest", j.id),
                    &public,
                    &child_auth,
                    &format!("herdr-guest-{}", &j.id[..12]),
                )?;
                write(
                    &job_dir(&j.id)?.join("guest-bootstrap.sh"),
                    child.as_bytes(),
                )?;
                nitro::repair_guest(j)?;
            }
            resume(j)
        }
        action @ ("start" | "stop" | "delete") => {
            let text = if action == "delete" {
                "Delete this job's instance and boot disk? This cannot be undone."
            } else if action == "stop" {
                "Stop this job's instance? Running programs will stop; disks remain billable."
            } else {
                "Start this job's instance? Provider compute charges resume."
            };
            if confirm(text, &action.to_uppercase())? {
                provider::lifecycle(j, action)?;
                if action == "start" {
                    resume(j)?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

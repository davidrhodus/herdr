//! Browser-first AWS setup; existing credential profiles remain AWS CLI-owned.
use super::{aws_network, input, model::*, safe, transport::*};
use std::{io, process::Command, time::Duration};

fn cli(args: &[&str]) -> Command {
    let mut c = Command::new(crate::platform::cloud_aws_program());
    c.args(args)
        .env("AWS_PAGER", "")
        .env("AWS_CLI_AUTO_PROMPT", "off");
    c
}
pub(super) fn browser_login_supported(version: &str) -> bool {
    let Some(v) = version
        .split_whitespace()
        .find_map(|v| v.strip_prefix("aws-cli/"))
    else {
        return false;
    };
    let mut parts = v.split('.').filter_map(|v| v.parse::<u32>().ok());
    matches!((parts.next(),parts.next()),(Some(2),Some(minor)) if minor>=32)
}
fn ensure_cli() -> io::Result<String> {
    loop {
        if let Ok(bytes) = capture(cli(&["--version"]), &[], Duration::from_secs(15)) {
            return String::from_utf8(bytes).map_err(|_| error("Invalid AWS CLI version"));
        }
        println!("AWS CLI is needed once on this computer. Herdr can open the official installation page; after installation, return here to continue.");
        match input("1 Open installer instructions / 2 Retry / 0 Cancel", "1")?.as_str() {
            "1" => {
                let _ = crate::platform::open_url(crate::platform::cloud_aws_install_url())?;
            }
            "2" => {}
            "0" => return Err(error("AWS setup cancelled")),
            _ => continue,
        }
        if input("Press Return after installation, or type 0 to cancel", "")? == "0" {
            return Err(error("AWS setup cancelled"));
        }
    }
}
fn profiles() -> io::Result<Vec<String>> {
    let bytes = capture(
        cli(&["configure", "list-profiles"]),
        &[],
        Duration::from_secs(15),
    )?;
    let text = String::from_utf8(bytes).map_err(|_| error("Invalid AWS profile list"))?;
    Ok(text
        .lines()
        .filter(|s| token(s).is_ok())
        .take(128)
        .map(str::to_owned)
        .collect())
}
fn property(profile: &str, key: &str) -> Option<String> {
    capture(
        cli(&["configure", "get", key, "--profile", profile]),
        &[],
        Duration::from_secs(15),
    )
    .ok()
    .and_then(|b| String::from_utf8(b).ok())
    .map(|s| s.trim().to_owned())
    .filter(|s| !s.is_empty() && token(s).is_ok())
}
fn fresh_profile(profiles: &[String]) -> String {
    (1..)
        .map(|n| {
            if n == 1 {
                "herdr".into()
            } else {
                format!("herdr-{n}")
            }
        })
        .find(|p| !profiles.contains(p))
        .unwrap_or_else(|| "herdr".into())
}
fn base(profile: &str, region: &str) -> Account {
    Account {
        name: profile.into(),
        provider: Provider::Aws,
        configuration: profile.into(),
        principal: String::new(),
        location: region.into(),
        project: String::new(),
        image: "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-6.18-x86_64".into(),
        size: "t3.small".into(),
        subnet: String::new(),
        security_group: String::new(),
    }
}
pub(super) fn resolve(profile: &str, region: &str) -> io::Result<Account> {
    token(profile)?;
    token(region)?;
    let mut account = base(profile, region);
    account.principal = field(&aws(&account, &["sts", "get-caller-identity"])?, "Account")?.into();
    Ok(account)
}
fn sign_in(profile: &str, region: &str, version: &str) -> io::Result<()> {
    let existing_sso =
        property(profile, "sso_session").is_some() || property(profile, "sso_start_url").is_some();
    let args = if existing_sso {
        vec!["sso", "login", "--profile", profile, "--region", region]
    } else {
        if !browser_login_supported(version) {
            return Err(error("Browser sign-in needs AWS CLI 2.32 or newer. Update using the installer option, or choose the advanced SSO setup."));
        }
        vec!["login", "--profile", profile, "--region", region]
    };
    println!("Opening AWS sign-in. Finish in your browser, then return to Herdr. AWS stores the temporary credentials; Herdr never asks for access keys.");
    if !cli(&args).status()?.success() {
        return Err(error("AWS sign-in did not finish. No Herdr account or instance was created. Your AWS identity may require SignInLocalDevelopmentAccess; Identity Center users can choose SSO setup."));
    }
    Ok(())
}
pub(super) fn authenticate(account: &Account) -> io::Result<()> {
    if verify_account(account).is_ok() {
        return Ok(());
    }
    let version = ensure_cli()?;
    sign_in(&account.configuration, &account.location, &version)?;
    // Reauthentication may not silently change the saved billing identity.
    verify_account(account)
}
pub(super) fn configure() -> io::Result<Account> {
    let version = ensure_cli()?;
    let profiles = profiles()?;
    println!("\nAWS account · choose an existing profile or sign in with your browser.");
    for (i, p) in profiles.iter().enumerate() {
        println!("  {}  {}", i + 1, safe(p));
    }
    println!("  N  Sign in to AWS in browser\n  S  SSO / advanced account setup\n  U  Install / update AWS CLI\n  0  Cancel");
    let choice = input("Choose", if profiles.is_empty() { "n" } else { "1" })?;
    if choice == "0" {
        return Err(error("AWS setup cancelled"));
    }
    if choice.eq_ignore_ascii_case("u") {
        let _ = crate::platform::open_url(crate::platform::cloud_aws_install_url())?;
        return Err(error("Finish the AWS CLI installation, then select Create or Configure AWS again. Existing profiles are kept."));
    }
    if choice.eq_ignore_ascii_case("s") {
        super::configure_legacy(Provider::Aws)?;
        return super::choose_account(&Provider::Aws);
    }
    let new = choice.eq_ignore_ascii_case("n");
    let profile = if new {
        fresh_profile(&profiles)
    } else {
        let n = choice
            .parse::<usize>()
            .map_err(|_| error("Choose a listed profile or N to sign in"))?;
        profiles
            .get(n.wrapping_sub(1))
            .cloned()
            .ok_or_else(|| error("No such profile"))?
    };
    let region = property(&profile, "region").unwrap_or_else(|| "us-west-2".into());
    let region = input(
        "AWS region (Return keeps the detected/default region)",
        &region,
    )?;
    token(&region)?;
    let account = if new {
        sign_in(&profile, &region, &version)?;
        resolve(&profile, &region)?
    } else {
        match resolve(&profile, &region) {
            Ok(account) => account,
            Err(_) => {
                sign_in(&profile, &region, &version)?;
                resolve(&profile, &region)?
            }
        }
    };
    let mut s = settings()?;
    let mut account = if let Some(saved) = s.accounts.iter().find(|a| {
        a.provider == Provider::Aws
            && a.configuration == profile
            && a.principal == account.principal
            && a.location == region
    }) {
        saved.clone()
    } else {
        account
    };
    println!("Verified AWS account {} in {}. Image and size defaults are filled in; they appear in the final create review.",safe(&account.principal),safe(&account.location));
    if account.subnet.is_empty()
        || account.security_group.is_empty()
        || aws_network::validate(&account).is_err()
    {
        aws_network::select(&mut account)?;
    }
    s.accounts
        .retain(|old| !(old.provider == Provider::Aws && old.configuration == profile));
    s.accounts.push(account.clone());
    save(&root()?.join("accounts.json"), &s)?;
    println!("AWS setup saved. Continuing to your server…");
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_login_requires_current_cli_and_new_profiles_do_not_overwrite_existing_names() {
        assert!(!browser_login_supported("aws-cli/2.31.9 Python/3"));
        assert!(browser_login_supported("aws-cli/2.32.0 Python/3"));
        assert!(browser_login_supported("aws-cli/2.40.2"));
        assert!(!browser_login_supported("aws-cli/1.40.2"));
        assert_eq!(
            fresh_profile(&["herdr".into(), "herdr-2".into()]),
            "herdr-3"
        );
    }
    #[test]
    fn valid_profiles_resolve_without_login_or_resource_mutations() {
        let _context = crate::cli::cloud::tests::Context::new();
        super::super::transport::FAKE.with(|f| {
            *f.borrow_mut() = Some(Box::new(|c, _| {
                let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy()).collect();
                assert!(args.iter().any(|a| a == "get-caller-identity"));
                Ok(br#"{"Account":"123"}"#.to_vec())
            }))
        });
        let account = resolve("existing", "us-west-2").unwrap();
        assert_eq!(account.principal, "123");
        assert!(account.subnet.is_empty());
    }
}

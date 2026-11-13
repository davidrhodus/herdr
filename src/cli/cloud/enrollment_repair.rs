//! Repair is durable before either VM lifecycle or bootstrap metadata changes.
use super::{bootstrap, model::*, nitro, provider, tailscale_setup};
use std::io;

fn supports_reauth(job: &Job) -> bool {
    use base64::Engine;
    job.account.provider == Provider::Gcp
        || job.request["UserData"]
            .as_str()
            .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
            .is_some_and(|v| {
                v.windows(b"# HERDR-REAUTH ".len())
                    .any(|w| w == b"# HERDR-REAUTH ")
            })
}

pub(super) fn stage(job: &mut Job) -> io::Result<()> {
    if job.endpoint.is_some() {
        return Err(error(
            "This server is already ready; use its SSH repair instead",
        ));
    }
    if job.pending_action.is_some() {
        return Err(error(
            "Finish the pending instance action before enrollment repair",
        ));
    }
    if !supports_reauth(job) && settings()?.enrollment_mode() == Enrollment::Browser {
        return Err(error("This older AWS bootstrap supports OAuth-key refresh only. Browser re-enrollment requires a new instance, or SSH repair of the existing bootstrap. The VM was not changed."));
    }
    let current = tailscale_setup::ensure()?;
    if job
        .expected_tailnet
        .as_ref()
        .is_some_and(|expected| expected != &current)
    {
        return Err(error(
            "Reconnect to this job's original Tailscale network before repair",
        ));
    }
    let nonce = uuid()?;
    let public = bootstrap::identity(&job.id)?;
    let auth = bootstrap::enrollment(&settings()?)?;
    let parent_script = bootstrap::repair_script(
        &job.id,
        &public,
        &auth,
        &format!("herdr-{}", &job.id[..18]),
        &nonce,
    )?;
    let guest_script = if job.nitro {
        let auth = bootstrap::enrollment(&settings()?)?;
        Some(bootstrap::repair_script(
            &format!("{}-guest", job.id),
            &public,
            &auth,
            &format!("herdr-guest-{}", &job.id[..12]),
            &nonce,
        )?)
    } else {
        None
    };
    // No activated nonce is changed until the provider has accepted the script.
    job.expected_tailnet = Some(current);
    job.pending_enrollment_repair = Some(EnrollmentRepair {
        nonce,
        parent_script,
        guest_script,
        parent_updated: false,
    });
    job.phase("Enrollment repair staged")
}
pub(super) fn resume(job: &mut Job) -> io::Result<()> {
    let Some(mut intent) = job.pending_enrollment_repair.clone() else {
        return Ok(());
    };
    if job.endpoint.is_some() {
        return Err(error(
            "Cannot restart a ready server through enrollment repair",
        ));
    }
    // An interrupted repair may already have requested its stop/start operation.
    if job.pending_action.is_some() {
        provider::finish_pending(job)?;
    }
    if !intent.parent_updated {
        provider::repair_enrollment(job, &intent.parent_script)?;
        intent.parent_updated = true;
        job.reauth_nonce = supports_reauth(job).then(|| intent.nonce.clone());
        job.pending_enrollment_repair = Some(intent.clone());
        job.save()?;
        tailscale_setup::reset_browser(job)?;
    }
    if let Some(script) = &intent.guest_script {
        write(
            &job_dir(&job.id)?.join("guest-bootstrap.sh"),
            script.as_bytes(),
        )?;
        nitro::repair_guest(job)?;
    }
    job.pending_enrollment_repair = None;
    job.phase("Enrollment repair applied")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::cloud::transport;
    use serde_json::json;
    #[test]
    fn interrupted_repair_is_durable_and_activates_the_nonce_after_metadata_and_restart() {
        let _context = crate::cli::cloud::tests::Context::new();
        let mut job = crate::cli::cloud::tests::job();
        job.account.provider = Provider::Gcp;
        job.account.principal = "me@example.com".into();
        job.account.project = "project".into();
        job.account.location = "us-central1-a".into();
        job.resource = Some("123".into());
        job.pending_enrollment_repair = Some(EnrollmentRepair {
            nonce: "new-nonce".into(),
            parent_script: "# HERDR-REAUTH new-nonce".into(),
            guest_script: None,
            parent_updated: false,
        });
        job.save().unwrap();
        let mut restored: Job = read(&job_dir(&job.id).unwrap().join("job.json")).unwrap();
        assert!(restored.reauth_nonce.is_none());
        let id = job.id.clone();
        let steps = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = steps.clone();
        transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |command,input| {
            let args:Vec<_>=command.get_args().map(|a|a.to_string_lossy()).collect();
            if command.get_program()=="gcloud" {return Ok(if args.iter().any(|a|a=="get-value"){b"me@example.com\n".to_vec()}else{b"token\n".to_vec()});}
            let text=std::str::from_utf8(input).unwrap();
            let response=if text.contains("/setMetadata?") {seen.borrow_mut().push("metadata");json!({"name":"metadata-op"})}
                else if text.contains("/reset?") {seen.borrow_mut().push("restart");json!({"name":"restart-op"})}
                else if text.contains("/operations/") {json!({"status":"DONE"})}
                else {json!({"id":"123","status":"RUNNING","labels":{"herdr-job":id},"metadata":{"fingerprint":"fp","items":[]}})};
            let mut bytes=serde_json::to_vec(&response).unwrap();bytes.extend_from_slice(b"\n200");Ok(bytes)
        })));
        resume(&mut restored).unwrap();
        assert_eq!(*steps.borrow(), vec!["metadata", "restart"]);
        assert_eq!(restored.reauth_nonce.as_deref(), Some("new-nonce"));
        assert!(restored.pending_enrollment_repair.is_none());
    }
}

use super::{model::*, transport::*};
use serde_json::{json, Value};
use std::{
    io,
    time::{Duration, Instant},
};

pub(super) fn create_request(
    account: &Account,
    id: &str,
    name: &str,
    bootstrap: &str,
    public: &str,
    nitro: bool,
) -> io::Result<Value> {
    token(name)?;
    if name.len() > 50
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !name.starts_with(|c: char| c.is_ascii_lowercase())
    {
        return Err(error("Instance name must start with a lowercase letter and contain at most 50 lowercase letters, digits, or hyphens"));
    }
    match account.provider {
        Provider::Aws => {
            super::aws_network::validate(account)?;
            // Resolve the exact AMI once. A moving SSM alias must not change a retried request.
            let image = if account.image.starts_with("/aws/") {
                field(
                    &aws(account, &["ssm", "get-parameter", "--name", &account.image])?
                        ["Parameter"],
                    "Value",
                )?
                .to_owned()
            } else {
                account.image.clone()
            };
            let groups = aws(
                account,
                &[
                    "ec2",
                    "describe-security-groups",
                    "--group-ids",
                    &account.security_group,
                ],
            )?;
            if !groups["SecurityGroups"][0]["IpPermissions"]
                .as_array()
                .is_some_and(Vec::is_empty)
            {
                return Err(error("Choose a security group with no inbound rules. Herdr connects through Tailscale; public SSH is unnecessary."));
            }
            let image_info = aws(account, &["ec2", "describe-images", "--image-ids", &image])?;
            let root_device = field(&image_info["Images"][0], "RootDeviceName")?;
            use base64::Engine;
            Ok(
                json!({"ImageId":image,"InstanceType":account.size,"MinCount":1,"MaxCount":1,
                "ClientToken":id,"UserData":base64::engine::general_purpose::STANDARD.encode(bootstrap),
                "MetadataOptions":{"HttpTokens":"required","HttpPutResponseHopLimit":1},
                "EnclaveOptions":{"Enabled":nitro},
                "BlockDeviceMappings":[{"DeviceName":root_device,"Ebs":{"VolumeSize":if nitro {64}else{30},"VolumeType":"gp3","Encrypted":true,"DeleteOnTermination":true}}],
                "NetworkInterfaces":[{"DeviceIndex":0,"SubnetId":account.subnet,"Groups":[account.security_group],"AssociatePublicIpAddress":true}],
                "TagSpecifications":[{"ResourceType":"instance","Tags":[{"Key":"Name","Value":name},{"Key":"herdr-job","Value":id}]},
                {"ResourceType":"volume","Tags":[{"Key":"herdr-job","Value":id}]}]}),
            )
        }
        Provider::Gcp => {
            let network = json(&google(
                account,
                &[
                    "compute",
                    "networks",
                    "subnets",
                    "describe",
                    &account.subnet,
                    "--region",
                    &gcp_region(&account.location)?,
                    "--format=json",
                ],
            )?)?;
            let subnet = field(&network, "selfLink")?;
            Ok(
                json!({"name":name,"machineType":format!("zones/{}/machineTypes/{}",account.location,account.size),
            "labels":{"herdr-job":id},"tags":{"items":[format!("herdr-{id}")]},
            "disks":[{"boot":true,"autoDelete":true,"initializeParams":{"sourceImage":account.image,"diskSizeGb":"30"}}],
            "networkInterfaces":[{"subnetwork":subnet,"network":network["network"],"stackType":"IPV4_ONLY","accessConfigs":[{"name":"External NAT","type":"ONE_TO_ONE_NAT"}]}],
            "metadata":{"items":[{"key":"startup-script","value":bootstrap},{"key":"block-project-ssh-keys","value":"TRUE"},
                {"key":"enable-oslogin","value":"FALSE"},{"key":"ssh-keys","value":format!("herdr:{public}")}]} }),
            )
        }
    }
}
pub(super) fn resource_identity(job: &Job, value: &Value) -> io::Result<String> {
    match job.account.provider {
        Provider::Aws => {
            if !value["Tags"].as_array().is_some_and(|tags| {
                tags.iter()
                    .any(|tag| tag["Key"] == "herdr-job" && tag["Value"] == job.id)
            }) {
                return Err(error("EC2 instance ownership does not match this job"));
            }
            Ok(field(value, "InstanceId")?.into())
        }
        Provider::Gcp => {
            if value["labels"]["herdr-job"] != job.id {
                return Err(error("GCP instance ownership does not match this job"));
            }
            Ok(field(value, "id")?.into())
        }
    }
}
pub(super) fn inspect(job: &Job) -> io::Result<Value> {
    let value = match job.account.provider {
        Provider::Aws => {
            let filter = format!("Name=tag:herdr-job,Values={}", job.id);
            let v = aws(
                &job.account,
                &["ec2", "describe-instances", "--filters", &filter],
            )?;
            let rows: Vec<_> = v["Reservations"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|r| r["Instances"].as_array().into_iter().flatten())
                .collect();
            if rows.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No instance found for this job",
                ));
            }
            if rows.len() != 1 {
                return Err(error("Expected exactly one instance for this job. Create outcome is uncertain; inspect the provider account before retrying. No second instance was created."));
            }
            rows[0].clone()
        }
        Provider::Gcp => gcp(
            &job.account,
            "GET",
            &format!("instances/{}", job.name),
            None,
        )?,
    };
    let id = resource_identity(job, &value)?;
    if job.resource.as_ref().is_some_and(|saved| saved != &id) {
        return Err(error(
            "The instance was replaced. Refusing to change a different resource.",
        ));
    }
    Ok(value)
}
pub(super) fn create(job: &mut Job) -> io::Result<()> {
    verify_account(&job.account)?;
    if job.deleted {
        return Err(error("This job was deleted; create a new job"));
    }
    let retry = if job.submitted && job.resource.is_none() {
        match inspect(job) {
            Ok(value) => {
                job.resource = Some(resource_identity(job, &value)?);
                job.save()?;
                false
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !retry_window(job, now()?) {
                    return Err(error("The create outcome is unknown and its idempotency window expired. Inspect the original cloud account before creating a replacement."));
                }
                true
            }
            Err(e) => return Err(e),
        }
    } else {
        false
    };
    if !job.submitted || retry {
        // Persist before the request. A crash or lost reply always takes the reconcile path.
        if job.account.provider == Provider::Gcp {
            ensure_firewall(job)?;
        }
        job.first_submission = Some(job.first_submission.unwrap_or(now()?));
        job.submitted = true;
        job.phase("Creating instance")?;
        match job.account.provider {
            Provider::Aws => {
                super::aws_network::validate(&job.account)?;
                let path = job_dir(&job.id)?.join("request.json");
                save(&path, &job.request)?;
                let arg = format!("file://{}", path.display());
                let value = aws(
                    &job.account,
                    &["ec2", "run-instances", "--cli-input-json", &arg],
                )?;
                job.resource = Some(field(&value["Instances"][0], "InstanceId")?.into());
            }
            Provider::Gcp => {
                let operation = gcp(
                    &job.account,
                    "POST",
                    &format!("instances?requestId={}", job.id),
                    Some(&job.request),
                )?;
                job.operation = Some(field(&operation, "name")?.into());
            }
        }
        job.save()?;
    }
    wait_running(job)
}
fn retry_window(job: &Job, now: u64) -> bool {
    job.first_submission
        .and_then(|first| now.checked_sub(first))
        .is_some_and(|age| age < 23 * 60 * 60)
}
fn gcp_region(zone: &str) -> io::Result<String> {
    zone.rsplit_once('-')
        .map(|(r, _)| r.to_owned())
        .ok_or_else(|| error("Enter a GCP zone such as us-central1-a"))
}
fn global(account: &Account, method: &str, path: &str, body: Option<&Value>) -> io::Result<Value> {
    let access = String::from_utf8(google(account, &["auth", "print-access-token"])?)
        .map_err(|_| error("Invalid Google token"))?;
    let url = format!(
        "https://compute.googleapis.com/compute/v1/projects/{}/global/{path}",
        account.project
    );
    http(method, &url, Some(access.trim()), body, None)
}
fn firewall_matches(job: &Job, rule: &Value) -> bool {
    rule["network"] == job.request["networkInterfaces"][0]["network"]
        && rule["description"] == format!("herdr-job:{}", job.id)
        && rule["priority"] == 0
        && rule["direction"] == "INGRESS"
        && rule["disabled"].as_bool() != Some(true)
        && rule["sourceRanges"] == json!(["0.0.0.0/0"])
        && rule["targetTags"] == json!([format!("herdr-{}", job.id)])
        && rule["denied"] == json!([{"IPProtocol":"tcp","ports":["22"]}])
}
fn ensure_firewall(job: &Job) -> io::Result<()> {
    let path = format!("firewalls/herdr-{}", job.id);
    if let Ok(rule) = global(&job.account, "GET", &path, None) {
        if !firewall_matches(job, &rule) {
            return Err(error(
                "Existing firewall rule is not owned by this job or does not block public SSH",
            ));
        }
        return Ok(());
    }
    let network = json(&google(
        &job.account,
        &[
            "compute",
            "networks",
            "subnets",
            "describe",
            &job.account.subnet,
            "--region",
            &gcp_region(&job.account.location)?,
            "--format=json",
        ],
    )?)?;
    let body = json!({"name":format!("herdr-{}",job.id),"network":field(&network,"network")?,
        "direction":"INGRESS","priority":0,"sourceRanges":["0.0.0.0/0"],
        "targetTags":[format!("herdr-{}",job.id)],"denied":[{"IPProtocol":"tcp","ports":["22"]}],
        "description":format!("herdr-job:{}",job.id)});
    let op = global(
        &job.account,
        "POST",
        &format!("firewalls?requestId={}", job.id),
        Some(&body),
    )?;
    let op = field(&op, "name")?;
    let start = Instant::now();
    while !operation_done(&global(
        &job.account,
        "GET",
        &format!("operations/{op}"),
        None,
    )?)? {
        if start.elapsed() > Duration::from_secs(180) {
            return Err(error("Firewall setup pending; Resume the saved job"));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    if !firewall_matches(job, &global(&job.account, "GET", &path, None)?) {
        return Err(error("Cloud SSH deny rule was not verified"));
    }
    Ok(())
}
pub(super) fn operation_done(value: &Value) -> io::Result<bool> {
    if value.get("error").is_some_and(|v| !v.is_null()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GCP operation failed. Inspect the retained job in the provider console.",
        ));
    }
    Ok(value["status"] == "DONE")
}
fn wait_running(job: &mut Job) -> io::Result<()> {
    let start = Instant::now();
    loop {
        if let Some(operation) = &job.operation {
            let v = gcp(
                &job.account,
                "GET",
                &format!("operations/{operation}"),
                None,
            )?;
            if !operation_done(&v)? {
                if start.elapsed() > Duration::from_secs(600) {
                    return Err(error("Still creating; Resume this job later"));
                }
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        }
        let v = inspect(job)?;
        job.resource = Some(resource_identity(job, &v)?);
        job.save()?;
        let state = match job.account.provider {
            Provider::Aws => v["State"]["Name"].as_str(),
            Provider::Gcp => v["status"].as_str(),
        };
        if matches!(state, Some("running" | "RUNNING")) {
            return job.phase("Waiting for Tailscale enrollment");
        }
        if matches!(state, Some("terminated" | "shutting-down" | "TERMINATED")) {
            return Err(error(
                "Instance is stopped or terminated; use Start if available",
            ));
        }
        if start.elapsed() > Duration::from_secs(600) {
            return Err(error("Instance is not running yet; Resume this job later"));
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}
pub(super) fn lifecycle(job: &mut Job, action: &str) -> io::Result<()> {
    if !matches!(action, "start" | "stop" | "delete") {
        return Err(error("Unknown lifecycle action"));
    }
    verify_account(&job.account)?;
    if let Some(pending) = &job.pending_action {
        if pending.name != action {
            return Err(error("Resume the pending lifecycle action first"));
        }
    } else {
        match inspect(job) {
            Ok(value) => {
                job.resource = Some(resource_identity(job, &value)?);
            }
            Err(e) if action == "delete" && e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        job.pending_action = Some(Lifecycle {
            name: action.into(),
            token: uuid()?,
            operation: None,
            cleanup_operation: None,
        });
        job.phase(&format!("{action} requested"))?;
    }
    finish_pending(job)?;
    Ok(())
}
fn wait_operation(job: &Job, operation: &str) -> io::Result<()> {
    let start = Instant::now();
    while !operation_done(&gcp(
        &job.account,
        "GET",
        &format!("operations/{operation}"),
        None,
    )?)? {
        if start.elapsed() > Duration::from_secs(600) {
            return Err(error("Lifecycle operation pending; Resume this saved job"));
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Ok(())
}
fn delete_firewall(job: &mut Job) -> io::Result<()> {
    let path = format!("firewalls/herdr-{}", job.id);
    let rule = match global(&job.account, "GET", &path, None) {
        Ok(rule) => rule,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !firewall_matches(job, &rule) {
        return Err(error(
            "Refusing to remove a firewall rule that changed ownership or settings",
        ));
    }
    let pending = job
        .pending_action
        .clone()
        .ok_or_else(|| error("Missing cleanup action"))?;
    let op = if let Some(op) = pending.cleanup_operation {
        op
    } else {
        let result = global(
            &job.account,
            "DELETE",
            &format!("{path}?requestId={}", pending.token),
            None,
        )?;
        let op = field(&result, "name")?.to_owned();
        if let Some(action) = job.pending_action.as_mut() {
            action.cleanup_operation = Some(op.clone());
        }
        job.save()?;
        op
    };
    let start = Instant::now();
    while !operation_done(&global(
        &job.account,
        "GET",
        &format!("operations/{op}"),
        None,
    )?)? {
        if start.elapsed() > Duration::from_secs(180) {
            return Err(error("Firewall deletion pending; Resume this saved job"));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(())
}
pub(super) fn finish_pending(job: &mut Job) -> io::Result<bool> {
    let action = job.pending_action.as_ref().map(|a| a.name.clone());
    let result = execute_pending(job);
    if result
        .as_ref()
        .is_err_and(|e| e.kind() == io::ErrorKind::InvalidInput)
    {
        job.pending_action = None;
        job.phase(&format!("{} failed", action.as_deref().unwrap_or("Action")))?;
    }
    result
}
fn execute_pending(job: &mut Job) -> io::Result<bool> {
    let Some(pending) = job.pending_action.clone() else {
        return Ok(false);
    };
    verify_account(&job.account)?;
    let missing = match inspect(job) {
        Ok(_) => false,
        Err(e) if pending.name == "delete" && e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };
    if !missing {
        let id = job
            .resource
            .as_deref()
            .ok_or_else(|| error("Missing captured resource ID"))?;
        match job.account.provider {
            Provider::Aws => {
                if job.nitro && pending.name != "start" && super::nitro::stop(job).is_err() {
                    println!("Guest is unreachable; the owned EC2 parent's shutdown will stop it.");
                }
                let command = match pending.name.as_str() {
                    "start" => "start-instances",
                    "stop" => "stop-instances",
                    "delete" => "terminate-instances",
                    _ => return Err(error("Unknown persisted action")),
                };
                aws(&job.account, &["ec2", command, "--instance-ids", id])?;
                if pending.name != "start" {
                    let start = Instant::now();
                    loop {
                        let value = inspect(job)?;
                        let state = value["State"]["Name"].as_str();
                        if state
                            == Some(if pending.name == "delete" {
                                "terminated"
                            } else {
                                "stopped"
                            })
                        {
                            break;
                        }
                        if start.elapsed() > Duration::from_secs(600) {
                            return Err(error(
                                "Lifecycle operation pending; Resume this saved job",
                            ));
                        }
                        std::thread::sleep(Duration::from_secs(3));
                    }
                }
            }
            Provider::Gcp => {
                let operation = if let Some(op) = &pending.operation {
                    op.clone()
                } else {
                    let (method, suffix) = if pending.name == "delete" {
                        (
                            "DELETE",
                            format!("instances/{}?requestId={}", job.name, pending.token),
                        )
                    } else {
                        (
                            "POST",
                            format!(
                                "instances/{}/{}?requestId={}",
                                job.name, pending.name, pending.token
                            ),
                        )
                    };
                    let result = gcp(&job.account, method, &suffix, None)?;
                    let op = field(&result, "name")?.to_owned();
                    if let Some(action) = job.pending_action.as_mut() {
                        action.operation = Some(op.clone());
                    }
                    job.save()?;
                    op
                };
                wait_operation(job, &operation)?;
            }
        }
    }
    if pending.name == "delete" {
        if job.account.provider == Provider::Gcp {
            delete_firewall(job)?;
        }
        job.deleted = true;
    }
    job.pending_action = None;
    job.phase(&format!("{} complete", pending.name))?;
    Ok(pending.name != "start")
}
pub(super) fn console(job: &Job) -> io::Result<String> {
    let bytes = match job.account.provider {
        Provider::Aws => {
            let id = job
                .resource
                .as_deref()
                .ok_or_else(|| error("Missing EC2 instance ID"))?;
            let v = aws(
                &job.account,
                &["ec2", "get-console-output", "--instance-id", id, "--latest"],
            )?;
            decode_console(field(&v, "Output")?)?
        }
        Provider::Gcp => {
            let v = gcp(
                &job.account,
                "GET",
                &format!("instances/{}/serialPort?port=1", job.name),
                None,
            )?;
            field(&v, "contents")?.as_bytes().to_vec()
        }
    };
    String::from_utf8(bytes).map_err(|_| error("Invalid serial console data"))
}
pub(super) fn repair_enrollment(job: &mut Job, script: &str) -> io::Result<()> {
    verify_account(&job.account)?;
    let value = inspect(job)?;
    let id = job
        .resource
        .clone()
        .ok_or_else(|| error("Resume the original creation before repairing enrollment"))?;
    match job.account.provider {
        Provider::Aws => {
            if value["State"]["Name"] != "stopped" {
                lifecycle(job, "stop")?;
            }
            use base64::Engine;
            let request = json!({"InstanceId":id,"UserData":{"Value":base64::engine::general_purpose::STANDARD.encode(script)}});
            let path = job_dir(&job.id)?.join("enrollment-repair.json");
            save(&path, &request)?;
            aws(
                &job.account,
                &[
                    "ec2",
                    "modify-instance-attribute",
                    "--cli-input-json",
                    &format!("file://{}", path.display()),
                ],
            )?;
            lifecycle(job, "start")?;
        }
        Provider::Gcp => {
            let mut metadata = value["metadata"].clone();
            let items = metadata["items"]
                .as_array_mut()
                .ok_or_else(|| error("Missing instance metadata"))?;
            items.retain(|item| item["key"] != "startup-script");
            items.push(json!({"key":"startup-script","value":script}));
            let operation = gcp(
                &job.account,
                "POST",
                &format!("instances/{}/setMetadata?requestId={}", job.name, uuid()?),
                Some(&metadata),
            )?;
            wait_operation(job, field(&operation, "name")?)?;
        }
    }
    Ok(())
}
fn decode_console(value: &str) -> io::Result<Vec<u8>> {
    use base64::Engine;
    // CLI versions may already decode the API's base64 string.
    if value.contains('\n') || value.starts_with("HERDR-READY ") {
        return Ok(value.as_bytes().to_vec());
    }
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| error("Invalid EC2 console encoding"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job() -> Job {
        serde_json::from_value(json!({"id":"test","account":{"name":"test","provider":"Gcp","configuration":"test","principal":"me@example.com","location":"us-central1-a","project":"test","image":"image","size":"e2-small","subnet":"default","security_group":""},"name":"test","nitro":false,"request":{},"resource":"123","operation":null,"submitted":true,"phase":"Creating","target":null,"endpoint":null,"deleted":false})).unwrap()
    }
    #[test]
    fn done_operations_with_errors_are_failures() {
        assert!(operation_done(&json!({"status":"DONE","error":{"errors":[{}]}})).is_err());
        assert!(operation_done(&json!({"status":"DONE"})).unwrap());
    }
    #[test]
    fn ownership_and_numeric_identity_are_both_required() {
        let j = job();
        assert!(
            resource_identity(&j, &json!({"id":"123","labels":{"herdr-job":"foreign"}})).is_err()
        );
        assert_eq!(
            resource_identity(&j, &json!({"id":"456","labels":{"herdr-job":"test"}})).unwrap(),
            "456"
        );
        // inspect compares this numeric ID with the recorded one before mutation.
        assert_ne!(j.resource.as_deref(), Some("456"));
    }
}

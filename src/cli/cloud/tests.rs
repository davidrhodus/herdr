//! Simulated provider contracts: no credentials, cloud calls, or paid resources.
use super::*;
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc};

pub(super) struct Context(std::path::PathBuf);
impl Context {
    pub(super) fn new() -> Self {
        let path = crate::platform::create_remote_ssh_config_dir("cloud-contract-test").unwrap();
        model::TEST_ROOT.with(|r| *r.borrow_mut() = Some(path.clone()));
        Self(path)
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        super::TEST_INPUT.with(|v| *v.borrow_mut() = None);
        super::tailscale_setup::TEST_STATUS.with(|v| *v.borrow_mut() = None);
        transport::FAKE.with(|f| *f.borrow_mut() = None);
        model::TEST_ROOT.with(|r| *r.borrow_mut() = None);
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn first_create_guides_missing_account_setup_then_returns_to_the_same_create_review() {
    let _context = Context::new();
    super::tailscale_setup::TEST_STATUS.with(|v| {
        *v.borrow_mut() = Some(
            json!({"BackendState":"Running","CurrentTailnet":{"MagicDNSSuffix":"tail.example"}}),
        )
    });
    super::TEST_INPUT.with(|v| {
        *v.borrow_mut() = Some(
            ["1", "", "1", "1", "1", "first-server", "", "", ""]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
    });
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(|c,_| {
        let a=args(c);
        if let Some(value)=super::aws_network::tests::fixture(c) {return Ok(value);}
        if a==["--version"] {return Ok(b"aws-cli/2.40.2".to_vec());}
        if a.iter().any(|a|a=="list-profiles") {return Ok(b"existing\n".to_vec());}
        if a.iter().any(|a|a=="get") {return Ok(b"us-west-2\n".to_vec());}
        if a.iter().any(|a|a=="get-caller-identity") {return Ok(bytes(json!({"Account":"123"})));}
        if a.iter().any(|a|a=="describe-vpcs") {return Ok(bytes(json!({"Vpcs":[{"VpcId":"vpc-test","State":"available","CidrBlock":"10.0.0.0/16"}]})));}
        panic!("must not log in again or create cloud resources: {a:?}")
    })));
    super::create(Provider::Aws, false).unwrap();
    assert_eq!(settings().unwrap().accounts.len(), 1);
    assert_eq!(settings().unwrap().accounts[0].configuration, "existing");
    assert!(super::TEST_INPUT.with(|v| v.borrow().as_ref().unwrap().is_empty()));
    assert!(jobs().unwrap().is_empty()); // User cancelled the final create review.
}
pub(super) fn job() -> Job {
    serde_json::from_value(json!({"id":"12345678-1234-4234-8234-123456789abc","account":{"name":"test","provider":"Aws","configuration":"test","principal":"123","location":"us-west-2","project":"","image":"ami-test","size":"t3.small","subnet":"subnet-test","security_group":"sg-test"},"name":"server","nitro":false,"request":{"ClientToken":"12345678-1234-4234-8234-123456789abc","UserData":"dummy"},"resource":null,"operation":null,"submitted":false,"phase":"Reviewed","target":null,"endpoint":null,"deleted":false})).unwrap()
}
fn bytes(v: Value) -> Vec<u8> {
    serde_json::to_vec(&v).unwrap()
}
fn args(c: &Command) -> Vec<String> {
    c.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}
#[test]
fn lost_create_reply_is_reconciled_without_a_second_create() {
    let _context = Context::new();
    let creates = Rc::new(RefCell::new(0));
    let calls = creates.clone();
    let id = job().id;
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |c,_| {
        let a=args(c);
        if let Some(data)=super::aws_network::tests::fixture(c) {return Ok(data);}
        if a.iter().any(|a|a=="get-caller-identity") {return Ok(bytes(json!({"Account":"123"})));}
        if a.iter().any(|a|a=="run-instances") {*calls.borrow_mut()+=1;return Err(error("lost reply"));}
        assert!(a.iter().any(|a|a=="describe-instances"));
        Ok(bytes(json!({"Reservations":[{"Instances":[{"InstanceId":"i-owned","Tags":[{"Key":"herdr-job","Value":id}],"State":{"Name":"running"}}]}]})))
    })));
    let mut j = job();
    assert!(provider::create(&mut j).is_err());
    let mut restored: Job = read(&job_dir(&j.id).unwrap().join("job.json")).unwrap();
    assert!(restored.submitted);
    provider::create(&mut restored).unwrap();
    assert_eq!(*creates.borrow(), 1);
    assert_eq!(restored.resource.as_deref(), Some("i-owned"));
}
#[test]
fn crash_before_send_retries_the_identical_frozen_request() {
    let _context = Context::new();
    let creates = Rc::new(RefCell::new(0));
    let calls = creates.clone();
    let expected = job().request;
    let id = job().id;
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |c,_| {
        let a=args(c);
        if let Some(data)=super::aws_network::tests::fixture(c) {return Ok(data);}
        if a.iter().any(|a|a=="get-caller-identity") {return Ok(bytes(json!({"Account":"123"})));}
        if a.iter().any(|a|a=="run-instances") {
            let path=a.last().unwrap().strip_prefix("file://").unwrap();
            let request:Value=read(std::path::Path::new(path)).unwrap(); assert_eq!(request,expected);
            *calls.borrow_mut()+=1;return Ok(bytes(json!({"Instances":[{"InstanceId":"i-owned"}]})));
        }
        assert!(a.iter().any(|a|a=="describe-instances"));
        Ok(bytes(if *calls.borrow()==0 {json!({"Reservations":[]})} else {json!({"Reservations":[{"Instances":[{"InstanceId":"i-owned","Tags":[{"Key":"herdr-job","Value":id}],"State":{"Name":"running"}}]}]})}))
    })));
    let mut j = job();
    j.submitted = true;
    j.first_submission = Some(now().unwrap());
    j.save().unwrap();
    provider::create(&mut j).unwrap();
    assert_eq!(*creates.borrow(), 1);
}
#[test]
fn account_drift_blocks_all_resource_requests() {
    let _context = Context::new();
    transport::FAKE.with(|f| {
        *f.borrow_mut() = Some(Box::new(|c, _| {
            assert!(args(c).iter().any(|a| a == "get-caller-identity"));
            Ok(bytes(json!({"Account":"other"})))
        }))
    });
    assert!(provider::create(&mut job()).is_err());
}
#[test]
fn deleting_a_foreign_resource_never_sends_a_mutation() {
    let _context = Context::new();
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(|c,_| {
        let a=args(c);
        if let Some(data)=super::aws_network::tests::fixture(c) {return Ok(data);}
        if a.iter().any(|a|a=="get-caller-identity") {return Ok(bytes(json!({"Account":"123"})));}
        assert!(a.iter().any(|a|a=="describe-instances"));
        Ok(bytes(json!({"Reservations":[{"Instances":[{"InstanceId":"i-foreign","Tags":[{"Key":"herdr-job","Value":"foreign"}]}]}]})))
    })));
    assert!(provider::lifecycle(&mut job(), "delete").is_err());
}
#[test]
fn deleting_a_stopped_nitro_parent_requires_no_guest_ssh() {
    let _context = Context::new();
    let deleted = Rc::new(RefCell::new(false));
    let state = deleted.clone();
    let id = job().id;
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |c,_| {
        assert_eq!(c.get_program(),"aws");let a=args(c);
        if a.iter().any(|a|a=="get-caller-identity") {return Ok(bytes(json!({"Account":"123"})));}
        if a.iter().any(|a|a=="terminate-instances") {*state.borrow_mut()=true;return Ok(bytes(json!({})));}
        assert!(a.iter().any(|a|a=="describe-instances"));
        Ok(bytes(json!({"Reservations":[{"Instances":[{"InstanceId":"i-owned","Tags":[{"Key":"herdr-job","Value":id}],"State":{"Name":if *state.borrow(){"terminated"}else{"stopped"}}}]}]})))
    })));
    let mut j = job();
    j.nitro = true;
    provider::lifecycle(&mut j, "delete").unwrap();
    assert!(j.deleted);
}

fn curl_body(input: &[u8]) -> Value {
    let config = std::str::from_utf8(input).unwrap();
    let value = config
        .lines()
        .find_map(|line| line.strip_prefix("data = "))
        .unwrap();
    let text: String = serde_json::from_str(value).unwrap();
    serde_json::from_str(&text).unwrap()
}
fn response(v: Value) -> Vec<u8> {
    let mut b = bytes(v);
    b.extend_from_slice(b"\n200");
    b
}
#[test]
fn gcp_create_waits_for_an_ipv4_firewall_then_tracks_the_instance_operation() {
    let _context = Context::new();
    let stage = Rc::new(RefCell::new(0));
    let state = stage.clone();
    let mut j = job();
    j.account.provider = Provider::Gcp;
    j.account.principal = "me@example.com".into();
    j.account.project = "project".into();
    j.account.location = "us-central1-a".into();
    j.account.subnet = "default".into();
    j.request = json!({"networkInterfaces":[{"network":"network-link"}]});
    let id = j.id.clone();
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |c,input| {
        if c.get_program()=="gcloud" {
            let a=args(c);
            if a.iter().any(|a|a=="get-value") {return Ok(b"me@example.com\n".to_vec());}
            if a.iter().any(|a|a=="print-access-token") {return Ok(b"dummy-secret-token\n".to_vec());}
            assert!(a.iter().any(|a|a=="describe"));return Ok(bytes(json!({"network":"network-link"})));
        }
        assert_eq!(c.get_program(),"curl");assert!(!args(c).join(" ").contains("dummy-secret-token"));
        let config=std::str::from_utf8(input).unwrap();
        if config.contains("/global/firewalls/herdr-") {
            if *state.borrow()==0 {return Ok(b"{}\n404".to_vec());}
            return Ok(response(json!({"network":"network-link","description":format!("herdr-job:{id}"),"priority":0,"direction":"INGRESS","sourceRanges":["0.0.0.0/0"],"targetTags":[format!("herdr-{id}")],"denied":[{"IPProtocol":"tcp","ports":["22"]}]})));
        }
        if config.contains("/global/firewalls?") {
            assert_eq!(curl_body(input)["sourceRanges"],json!(["0.0.0.0/0"]));
            *state.borrow_mut()=1;return Ok(response(json!({"name":"firewall-op"})));
        }
        if config.contains("/operations/") {return Ok(response(json!({"status":"DONE"})));}
        if config.contains("/instances?") {
            assert_eq!(*state.borrow(),1);assert!(config.contains(&format!("requestId={id}")));
            *state.borrow_mut()=2;return Ok(response(json!({"name":"create-op"})));
        }
        assert!(config.contains("/instances/server"));
        Ok(response(json!({"id":"123","status":"RUNNING","labels":{"herdr-job":id}})))
    })));
    provider::create(&mut j).unwrap();
    assert_eq!(*stage.borrow(), 2);
    assert_eq!(j.resource.as_deref(), Some("123"));
}
#[test]
fn expired_unknown_create_is_not_replayed() {
    let _context = Context::new();
    transport::FAKE.with(|f| {
        *f.borrow_mut() = Some(Box::new(|c, _| {
            let a = args(c);
            if a.iter().any(|a| a == "get-caller-identity") {
                return Ok(bytes(json!({"Account":"123"})));
            }
            assert!(a.iter().any(|a| a == "describe-instances"));
            Ok(bytes(json!({"Reservations":[]})))
        }))
    });
    let mut j = job();
    j.submitted = true;
    j.first_submission = Some(now().unwrap() - 24 * 60 * 60);
    assert!(provider::create(&mut j)
        .unwrap_err()
        .to_string()
        .contains("window expired"));
}

fn google_job() -> Job {
    let mut j = job();
    j.account.provider = Provider::Gcp;
    j.account.principal = "me@example.com".into();
    j.account.project = "project".into();
    j.account.location = "us-central1-a".into();
    j.resource = Some("123".into());
    j.submitted = true;
    j.request = json!({"networkInterfaces":[{"network":"network-link"}]});
    j
}
fn google_auth(c: &Command) -> Option<Vec<u8>> {
    if c.get_program() != "gcloud" {
        return None;
    }
    Some(if args(c).iter().any(|a| a == "get-value") {
        b"me@example.com\n".to_vec()
    } else {
        b"dummy-token\n".to_vec()
    })
}

#[test]
fn running_gcp_instance_survives_expired_creation_and_start_operation_history() {
    let _context = Context::new();
    let mut j = google_job();
    j.operation = Some("expired-create".into());
    let id = j.id.clone();
    transport::FAKE.with(|f| {
        *f.borrow_mut() = Some(Box::new(move |c, input| {
            if let Some(bytes) = google_auth(c) {
                return Ok(bytes);
            }
            let config = std::str::from_utf8(input).unwrap();
            assert!(
                !config.contains("/operations/"),
                "owned live resource must not depend on expired history"
            );
            assert!(config.contains("/instances/"));
            assert!(config.contains("request = \"GET\""));
            Ok(response(
                json!({"id":"123","status":"RUNNING","labels":{"herdr-job":id}}),
            ))
        }))
    });
    provider::create(&mut j).unwrap();
    assert!(j.operation.is_none());
    j.pending_action = Some(Lifecycle {
        name: "start".into(),
        token: uuid().unwrap(),
        operation: Some("expired-start".into()),
        cleanup_operation: None,
    });
    provider::finish_pending(&mut j).unwrap();
    assert!(j.pending_action.is_none());
}
#[test]
fn a_failed_gcp_start_can_be_followed_by_explicit_delete() {
    let _context = Context::new();
    let mut j = google_job();
    let id = j.id.clone();
    let gone = Rc::new(RefCell::new(false));
    let state = gone.clone();
    j.pending_action = Some(Lifecycle {
        name: "start".into(),
        token: uuid().unwrap(),
        operation: Some("failed-op".into()),
        cleanup_operation: None,
    });
    transport::FAKE.with(|f| {
        *f.borrow_mut() = Some(Box::new(move |c, input| {
            if let Some(bytes) = google_auth(c) {
                return Ok(bytes);
            }
            let config = std::str::from_utf8(input).unwrap();
            if config.contains("failed-op") {
                return Ok(response(
                    json!({"status":"DONE","error":{"errors":[{"code":"QUOTA_EXCEEDED"}]}}),
                ));
            }
            if config.contains("/global/firewalls/") {
                return Ok(b"{}\n404".to_vec());
            }
            if config.contains("request = \"DELETE\"") {
                *state.borrow_mut() = true;
                return Ok(response(json!({"name":"delete-op"})));
            }
            if config.contains("/operations/delete-op") {
                return Ok(response(json!({"status":"DONE"})));
            }
            if *state.borrow() {
                return Ok(b"{}\n404".to_vec());
            }
            Ok(response(
                json!({"id":"123","status":"TERMINATED","labels":{"herdr-job":id}}),
            ))
        }))
    });
    assert!(provider::finish_pending(&mut j).is_err());
    assert!(j.pending_action.is_none());
    assert_eq!(j.phase, "start failed");
    provider::lifecycle(&mut j, "delete").unwrap();
    assert!(j.deleted);
}
#[test]
fn failed_async_firewall_cleanup_is_not_marked_deleted_and_can_be_retried() {
    let _context = Context::new();
    let mut j = google_job();
    let id = j.id.clone();
    let deletes = Rc::new(RefCell::new(0));
    let calls = deletes.clone();
    let tokens = Rc::new(RefCell::new(Vec::new()));
    let seen = tokens.clone();
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |c,input| {
        if let Some(bytes)=google_auth(c) {return Ok(bytes);}
        let config=std::str::from_utf8(input).unwrap();
        if config.contains("/instances/") {return Ok(b"{}\n404".to_vec());}
        if config.contains("request = \"DELETE\"") {
            *calls.borrow_mut()+=1;seen.borrow_mut().push(config.lines().find(|l|l.starts_with("url = ")).unwrap().to_owned());
            return Ok(response(json!({"name":"cleanup-op"})));
        }
        if config.contains("/operations/cleanup-op") {
            return Ok(response(if *calls.borrow()==1 {json!({"status":"DONE","error":{"errors":[{}]}})}else{json!({"status":"DONE"})}));
        }
        Ok(response(json!({"network":"network-link","description":format!("herdr-job:{id}"),"priority":0,"direction":"INGRESS","sourceRanges":["0.0.0.0/0"],"targetTags":[format!("herdr-{id}")],"denied":[{"IPProtocol":"tcp","ports":["22"]}]})))
    })));
    assert!(provider::lifecycle(&mut j, "delete").is_err());
    assert!(!j.deleted);
    assert!(j.pending_action.is_none());
    assert_eq!(j.phase, "delete failed");
    provider::lifecycle(&mut j, "delete").unwrap();
    assert!(j.deleted);
    assert_eq!(*deletes.borrow(), 2);
    assert_ne!(tokens.borrow()[0], tokens.borrow()[1]);
}
#[cfg(unix)]
#[test]
fn bootstrap_scripts_are_valid_bash() {
    for script in [include_str!("bootstrap.sh"), include_str!("nitro.sh")] {
        let mut command = Command::new("bash");
        command.arg("-n");
        assert!(capture(
            command,
            script.as_bytes(),
            std::time::Duration::from_secs(5)
        )
        .is_ok());
    }
}

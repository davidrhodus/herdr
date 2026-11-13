use super::*;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "herdr-private-update-{}",
            crate::client::endpoint::ProfileId::generate()
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn policy() -> Policy {
    Policy {
        version: 1,
        channel: "test".into(),
        address: "100.64.0.1:47383".parse().unwrap(),
        signer: "ssh-ed25519 AAAA test".into(),
        enabled: true,
    }
}
fn manifest() -> Manifest {
    Manifest {
        version: 1,
        channel: "test".into(),
        release: 10,
        target: "test-target".into(),
        build_id: "abc123".into(),
        sha256: "a".repeat(64),
        bytes: 4096,
        protocol: crate::protocol::PROTOCOL_VERSION,
        endpoint_generation: crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
    }
}
#[test]
fn incompatible_manifests_and_rollback_or_counter_conflicts_are_rejected() {
    let p = policy();
    let m = manifest();
    m.validate(&p, "test-target", None).unwrap();
    for n in 0..9 {
        let mut bad = m.clone();
        match n {
            0 => bad.release = 0,
            1 => bad.target = "wrong".into(),
            2 => bad.channel = "other".into(),
            3 => bad.sha256 = "../bad".into(),
            4 => bad.bytes = MAX_BINARY + 1,
            5 => bad.protocol += 1,
            6 => bad.endpoint_generation += 1,
            7 => bad.build_id = "bad\n".into(),
            _ => bad.version = 2,
        };
        assert!(bad.validate(&p, "test-target", None).is_err());
    }
    let mut accepted = m.receipt();
    m.validate(&p, "test-target", Some(&accepted)).unwrap();
    accepted.release = 11;
    assert!(m.validate(&p, "test-target", Some(&accepted)).is_err());
    let mut same_build = manifest();
    same_build.release = 12;
    same_build.sha256 = "b".repeat(64);
    assert!(same_build
        .validate(&p, "test-target", Some(&manifest().receipt()))
        .is_err());
    accepted.release = 10;
    accepted.sha256 = "b".repeat(64);
    assert!(m.validate(&p, "test-target", Some(&accepted)).is_err());
}
#[test]
fn promotion_keeps_previous_complete_binary_and_rejects_non_files() {
    let t = Temp::new();
    let src = t.0.join("new");
    let dest = t.0.join("herdr");
    fs::write(&src, b"new build").unwrap();
    fs::write(&dest, b"old build").unwrap();
    install::promote(&src, &dest).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"new build");
    assert_eq!(fs::read(t.0.join("herdr.previous")).unwrap(), b"old build");
    let directory = t.0.join("directory");
    fs::create_dir(&directory).unwrap();
    assert!(install::promote(&src, &directory).is_err());
}
#[test]
fn forged_receipts_and_replaced_binaries_cannot_request_restarts() {
    let t = Temp::new();
    let dest = t.0.join("herdr");
    fs::write(&dest, b"candidate").unwrap();
    save(&t.0.join("policy.json"), &policy()).unwrap();
    let mut r = manifest().receipt();
    r.sha256 = digest(&dest).unwrap();
    save(&t.0.join("installed.json"), &r).unwrap();
    assert!(!ready_at(&t.0, &dest, Some("old")).unwrap());
    save(&t.0.join("accepted.json"), &r).unwrap();
    assert!(ready_at(&t.0, &dest, Some("old")).unwrap());
    assert!(!ready_at(&t.0, &dest, Some(&r.build_id)).unwrap());
    fs::write(&dest, b"other").unwrap();
    assert!(!ready_at(&t.0, &dest, Some("old")).unwrap());
}
#[test]
fn channel_rejects_wildcard_public_or_unbounded_values() {
    let mut p = policy();
    p.validate().unwrap();
    for ip in ["0.0.0.0:1", "127.0.0.1:1", "8.8.8.8:1"] {
        p.address = ip.parse().unwrap();
        assert!(p.validate().is_err());
    }
    p = policy();
    p.signer = "ssh-ed25519 AAAA\nmalicious".into();
    assert!(p.validate().is_err());
}
#[test]
fn signature_verification_uses_pinned_key_and_exact_namespace() {
    use base64::Engine;
    let t = Temp::new();
    let key = t.0.join("signer");
    assert!(std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success());
    let mut p = policy();
    p.signer = fs::read_to_string(key.with_extension("pub"))
        .unwrap()
        .trim()
        .into();
    let payload = t.0.join("manifest");
    let bytes = serde_json::to_vec(&manifest()).unwrap();
    fs::write(&payload, &bytes).unwrap();
    assert!(std::process::Command::new("ssh-keygen")
        .args(["-Y", "sign", "-f"])
        .arg(&key)
        .args(["-n", NAMESPACE])
        .arg(&payload)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success());
    let signature = fs::read(payload.with_extension("sig")).unwrap();
    let envelope = Envelope {
        manifest: base64::engine::general_purpose::STANDARD.encode(&bytes),
        signature: base64::engine::general_purpose::STANDARD.encode(&signature),
    };
    assert_eq!(
        verify(&t.0, &p, &serde_json::to_vec(&envelope).unwrap()).unwrap(),
        manifest()
    );
    let other_namespace = t.0.join("other-namespace");
    fs::write(&other_namespace, &bytes).unwrap();
    assert!(std::process::Command::new("ssh-keygen")
        .args(["-Y", "sign", "-f"])
        .arg(&key)
        .args(["-n", "wrong-namespace"])
        .arg(&other_namespace)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success());
    let wrong = Envelope {
        manifest: envelope.manifest.clone(),
        signature: base64::engine::general_purpose::STANDARD
            .encode(fs::read(other_namespace.with_extension("sig")).unwrap()),
    };
    assert!(verify(&t.0, &p, &serde_json::to_vec(&wrong).unwrap()).is_err());
    let tampered = Envelope {
        manifest: base64::engine::general_purpose::STANDARD.encode(b"{}"),
        signature: envelope.signature,
    };
    assert!(verify(&t.0, &p, &serde_json::to_vec(&tampered).unwrap()).is_err());
}

#[test]
fn complete_signed_install_rejects_corruption_and_recovers_an_interrupted_receipt() {
    use base64::Engine;
    let t = Temp::new();
    let key = t.0.join("key");
    assert!(std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success());
    let mut p = policy();
    p.signer = fs::read_to_string(key.with_extension("pub"))
        .unwrap()
        .trim()
        .into();
    save(&t.0.join("policy.json"), &p).unwrap();
    let candidate = vec![b'x'; 4096];
    let mut m = manifest();
    m.sha256 = format!("{:x}", Sha256::digest(&candidate));
    let file = t.0.join("signed-manifest");
    fs::write(&file, serde_json::to_vec(&m).unwrap()).unwrap();
    assert!(std::process::Command::new("ssh-keygen")
        .args(["-Y", "sign", "-f"])
        .arg(&key)
        .args(["-n", NAMESPACE])
        .arg(&file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success());
    let envelope = serde_json::to_vec(&Envelope {
        manifest: base64::engine::general_purpose::STANDARD.encode(fs::read(&file).unwrap()),
        signature: base64::engine::general_purpose::STANDARD
            .encode(fs::read(file.with_extension("sig")).unwrap()),
    })
    .unwrap();
    let dest = t.0.join("herdr");
    fs::write(&dest, b"old complete build").unwrap();
    assert!(check_with(
        &t.0,
        &dest,
        &p,
        "test-target",
        || Err(fail("offline")),
        |_, _| panic!("must not download")
    )
    .is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"old complete build");
    assert!(check_with(
        &t.0,
        &dest,
        &p,
        "test-target",
        || Ok(envelope.clone()),
        |_, f| f.write_all(&vec![b'z'; 4096])
    )
    .is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"old complete build");
    assert!(accepted_at(&t.0).unwrap().is_none());
    assert!(check_with(
        &t.0,
        &dest,
        &p,
        "test-target",
        || Ok(envelope.clone()),
        |_, f| {
            f.write_all(b"partial")?;
            Err(fail("interrupted"))
        }
    )
    .is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"old complete build");
    assert!(check_with(
        &t.0,
        &dest,
        &p,
        "test-target",
        || Ok(envelope.clone()),
        |_, f| {
            let mut off = p.clone();
            off.enabled = false;
            save(&t.0.join("policy.json"), &off)?;
            f.write_all(&candidate)
        }
    )
    .is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"old complete build");
    save(&t.0.join("policy.json"), &p).unwrap();
    check_with(
        &t.0,
        &dest,
        &p,
        "test-target",
        || Ok(envelope.clone()),
        |_, f| f.write_all(&candidate),
    )
    .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), candidate);
    assert_eq!(
        fs::read(t.0.join("herdr.previous")).unwrap(),
        b"old complete build"
    );
    fs::remove_file(t.0.join("installed.json")).unwrap();
    check_with(
        &t.0,
        &dest,
        &p,
        "test-target",
        || Ok(envelope),
        |_, _| panic!("must recover without downloading again"),
    )
    .unwrap();
    assert_eq!(
        json::<Receipt>(&t.0.join("installed.json")).unwrap().sha256,
        m.sha256
    );
}

#[test]
fn registered_job_stale_or_wrong_worker_never_confirms_enable() {
    let expected = "a".repeat(64);
    let service = serde_json::json!({"running":true,"binary":expected,"private_updates":true,"heartbeat":100,"pid":123});
    let updater = serde_json::json!({"automatic":true,"pid":123,"error":null,"checked_at":100});
    assert!(ready_service(&service, &updater, &expected, 100));
    for n in 0..7 {
        let mut s = service.clone();
        let mut u = updater.clone();
        match n {
            0 => s["running"] = false.into(),
            1 => s["binary"] = "old".into(),
            2 => s["heartbeat"] = 99.into(),
            3 => s["private_updates"] = false.into(),
            4 => u["pid"] = 122.into(),
            5 => u["automatic"] = false.into(),
            _ => u["error"] = "network failed".into(),
        };
        assert!(!ready_service(&s, &u, &expected, 100), "case {n}");
    }
    assert!(!ready_service(
        &service,
        &serde_json::json!({}),
        &expected,
        100
    ));
}
#[test]
fn automatic_interval_is_shared_across_processes_and_clock_regressions_retry() {
    let v = serde_json::json!({"automatic":true,"checked_at":100});
    assert!(!automatic_due(&v, 105));
    assert!(!automatic_due(&v, 159));
    assert!(automatic_due(&v, 160));
    assert!(automatic_due(&v, 99));
    assert!(automatic_due(
        &serde_json::json!({"automatic":false,"checked_at":100}),
        101
    ));
}
#[test]
fn busy_update_check_does_not_claim_a_successful_check() {
    let t = Temp::new();
    let lease = crate::client::direct_sync::worker_lease(&t.0, "check.lock").unwrap();
    lease.lock().unwrap();
    assert!(!check_with(
        &t.0,
        &t.0.join("herdr"),
        &policy(),
        "test-target",
        || panic!("must not fetch while another checker owns lock"),
        |_, _| panic!()
    )
    .unwrap());
    assert!(!t.0.join("installed.json").exists());
}

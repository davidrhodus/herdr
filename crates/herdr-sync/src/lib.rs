//! Peer server-list sync, independent of terminal/runtime protocols.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_RECORDS: usize = 1024;
pub const MAX_MACHINES: usize = 64;
// Accommodates 64 profiles at their maximum validated field lengths,
// including JSON escaping and the native catalog wrapper.
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
pub const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub target: String,
    pub session: String,
    pub enabled: bool,
}

pub fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Machine {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err("invalid machine id".into());
        }
        for (value, max) in [
            (self.label.trim(), 128),
            (self.target.as_str(), 1024),
            (self.session.as_str(), 64),
        ] {
            if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
                return Err("invalid machine label, target, or session".into());
            }
        }
        if self.session == "."
            || self.session == ".."
            || !self
                .session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err("invalid session name".into());
        }
        let target = self.target.strip_prefix("ssh://").unwrap_or(&self.target);
        if target.starts_with('-')
            || target
                .rsplit_once('@')
                .is_some_and(|(user, _)| user.contains(':'))
        {
            return Err("SSH target must not contain a password or command option".into());
        }
        Ok(())
    }
}

/// Logical clocks avoid depending on either computer's wall clock.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub counter: u64,
    pub device: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub stamp: Stamp,
    /// Tombstones are permanent for this id; adding again creates a fresh id.
    pub machine: Option<Machine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u32,
    pub records: Vec<Record>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: VERSION,
            records: Vec::new(),
        }
    }
}

impl Document {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION || self.records.len() > MAX_RECORDS {
            return Err("unsupported sync version or too many historical records".into());
        }
        let mut ids = std::collections::HashSet::new();
        for record in &self.records {
            if !valid_id(&record.id)
                || !valid_id(&record.stamp.device)
                || record.stamp.counter == 0
                || record.stamp.counter == u64::MAX
                || !ids.insert(&record.id)
            {
                return Err("invalid or duplicate sync record".into());
            }
            if let Some(machine) = &record.machine {
                machine.validate()?;
                if machine.id != record.id {
                    return Err("machine id does not match record".into());
                }
            }
        }
        if self.machines().count() > MAX_MACHINES {
            return Err("at most 64 machines can be synchronized".into());
        }
        Ok(())
    }

    pub fn machines(&self) -> impl Iterator<Item = &Machine> {
        self.records.iter().filter_map(|r| r.machine.as_ref())
    }

    pub fn clock(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.stamp.counter)
            .max()
            .unwrap_or(0)
    }

    /// Idempotent, commutative merge. Deletes beat all edits to the same id;
    /// concurrent live edits use a deterministic clock/device ordering.
    pub fn merge(&self, other: &Self) -> Result<Self, String> {
        self.validate()?;
        other.validate()?;
        let mut records = BTreeMap::<String, Record>::new();
        for incoming in self.records.iter().chain(&other.records) {
            if let Some(current) = records.get(&incoming.id) {
                if current.stamp == incoming.stamp && current.machine != incoming.machine {
                    return Err("conflicting values for the same logical stamp".into());
                }
                let winner = match (current.machine.is_none(), incoming.machine.is_none()) {
                    (true, false) => current,
                    (false, true) => incoming,
                    _ if current.stamp >= incoming.stamp => current,
                    _ => incoming,
                };
                let winner = winner.clone();
                records.insert(winner.id.clone(), winner);
            } else {
                records.insert(incoming.id.clone(), incoming.clone());
            }
        }
        let merged = Self {
            version: VERSION,
            records: records.into_values().collect(),
        };
        merged.validate()?;
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(id: char, device: char, counter: u64, label: Option<&str>) -> Record {
        let id = id.to_string().repeat(32);
        Record {
            machine: label.map(|label| Machine {
                id: id.clone(),
                label: label.into(),
                target: "user@host".into(),
                session: "default".into(),
                enabled: true,
            }),
            id,
            stamp: Stamp {
                counter,
                device: device.to_string().repeat(32),
            },
        }
    }
    fn doc(records: Vec<Record>) -> Document {
        Document {
            records,
            ..Document::default()
        }
    }

    #[test]
    fn concurrent_additions_and_renames_converge_without_deduplicating_destinations() {
        let a = doc(vec![record('a', '1', 1, Some("Desktop"))]);
        let b = doc(vec![record('b', '2', 1, Some("Laptop"))]);
        let ab = a.merge(&b).unwrap();
        assert_eq!(ab.machines().count(), 2);
        assert_eq!(ab, b.merge(&a).unwrap());
        assert_eq!(ab, ab.merge(&ab).unwrap());
        let rename = doc(vec![record('a', '2', 2, Some("Renamed"))]);
        assert_eq!(ab.merge(&rename).unwrap(), rename.merge(&ab).unwrap());
        assert_eq!(
            ab.merge(&rename).unwrap().machines().next().unwrap().label,
            "Renamed"
        );
    }
    #[test]
    fn deletion_survives_stale_and_newer_offline_edits_and_out_of_order_retries() {
        let deleted = doc(vec![record('a', '1', 2, None)]);
        for clock in [1, 3, 1000] {
            let edit = doc(vec![record('a', '2', clock, Some("Offline rename"))]);
            let merged = deleted.merge(&edit).unwrap();
            assert_eq!(merged.machines().count(), 0);
            assert_eq!(merged, edit.merge(&deleted).unwrap());
            assert_eq!(merged, merged.merge(&edit).unwrap());
        }
    }
    #[test]
    fn malformed_or_secret_fields_and_clock_collisions_are_rejected() {
        let a = doc(vec![record('a', '1', 1, Some("A"))]);
        let b = doc(vec![record('a', '1', 1, Some("B"))]);
        assert!(a.merge(&b).is_err());
        let mut bad = a.clone();
        bad.records[0].machine.as_mut().unwrap().target = "ssh://user:password@host".into();
        assert!(bad.validate().is_err());
        let mut value = serde_json::to_value(a).unwrap();
        value["records"][0]["machine"]["private_key"] = "secret".into();
        assert!(serde_json::from_value::<Document>(value).is_err());
    }
    #[test]
    fn protocol_rejects_sessions_that_native_clients_cannot_load() {
        let mut value = doc(vec![record('a', '1', 1, Some("A"))]);
        for name in [".", "..", "bad session", "é", "../escape"] {
            value.records[0].machine.as_mut().unwrap().session = name.into();
            assert!(value.validate().is_err(), "{name}");
        }
    }
    #[test]
    fn padded_native_labels_sync_without_changing_their_stored_value() {
        for label in [
            " Build ".to_string(),
            "\nBuild\n".to_string(),
            format!("  {}  ", "x".repeat(128)),
        ] {
            let mut value = doc(vec![record('a', '1', 1, Some("A"))]);
            value.records[0].machine.as_mut().unwrap().label = label.clone();
            value.validate().unwrap();
            assert_eq!(value.machines().next().unwrap().label, label);
        }
    }
    #[test]
    fn fresh_device_cannot_erase_a_shared_catalog() {
        let existing = doc(vec![record('a', '1', 1, Some("A"))]);
        assert_eq!(existing.merge(&Document::default()).unwrap(), existing);
    }
}

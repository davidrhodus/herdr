//! Client-owned sequential setup plan. Credentials remain in the existing PTY.
use super::*;
use crate::client::machine_setup::TailscaleDevice;

pub(super) const LIMIT: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client::shell) enum State {
    Pending,
    Running,
    Success,
    Failed,
    Stopped,
    Skipped,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Waiting",
            Self::Running => "Setting up",
            Self::Success => "Ready",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Skipped => "Skipped",
        }
    }
}
#[derive(Debug)]
pub(in crate::client::shell) struct Item {
    pub device: TailscaleDevice,
    pub operation: Option<Operation>,
    pub expected: Option<SavedSshEndpoint>,
    pub state: State,
    pub detail: String,
    pub screen: Option<Buffer>,
}
#[derive(Debug)]
pub(in crate::client::shell) struct Batch {
    pub items: Vec<Item>,
    pub active: Option<usize>,
    pub done: bool,
}
impl Batch {
    pub fn plan(manager: &Manager) -> Result<Self, String> {
        if manager.tailscale.loading {
            return Err("Wait for device discovery to finish.".into());
        }
        if manager.tailscale.selected.is_empty() || manager.tailscale.selected.len() > LIMIT {
            return Err(format!("Select between 1 and {LIMIT} devices."));
        }
        let mut items = Vec::new();
        for (key, chosen) in &manager.tailscale.selected {
            let current = manager.tailscale.devices.iter().find(|d| {
                tailscale_browser::device_key(d).as_ref() == Some(key) && d.online == Some(true)
            });
            let mut item = Item {
                device: chosen.clone(),
                operation: None,
                expected: None,
                state: State::Skipped,
                detail: "Not online in the refreshed list; select it again when available.".into(),
                screen: None,
            };
            if let Some(device) = current {
                item.device = device.clone();
                let mut form = manager.form();
                form.target = device.target().ok_or("Device has no SSH address")?;
                form.label = device.name.clone();
                form.destination()?;
                let saved = tailscale_browser::existing_profiles(
                    &manager.profiles,
                    device,
                    form.username.trim(),
                );
                if saved.is_empty() {
                    item.operation = Some(Operation::AddChecked(form));
                } else if saved.len() == 1 && saved[0].enabled {
                    item.expected = Some(saved[0].clone());
                    item.operation = Some(Operation::ReconnectChecked(saved[0].clone()));
                } else {
                    item.detail =
                        "Saved server is disabled or has several sessions; choose it from Servers."
                            .into();
                }
                if item.operation.is_some() {
                    item.state = State::Pending;
                    item.detail.clear();
                }
            }
            items.push(item);
        }
        let done = items.iter().all(|i| i.state != State::Pending);
        Ok(Self {
            items,
            active: None,
            done,
        })
    }
    pub fn ready(&self) -> bool {
        !self.done && self.active.is_none()
    }
    pub fn next(&mut self, profiles: &[SavedSshEndpoint]) -> Option<Operation> {
        if !self.ready() {
            return None;
        }
        for (index, item) in self.items.iter_mut().enumerate() {
            if item.state != State::Pending {
                continue;
            }
            if let Some(expected) = &item.expected {
                if !profiles.iter().any(|p| {
                    p.id == expected.id
                        && p.target == expected.target
                        && p.session == expected.session
                        && p.enabled
                }) {
                    item.state = State::Failed;
                    item.detail = "Saved server changed; review it before retrying.".into();
                    continue;
                }
            } else if let Some(Operation::AddChecked(form)) = &item.operation {
                let saved = tailscale_browser::existing_profiles(
                    profiles,
                    &item.device,
                    form.username.trim(),
                );
                if saved.len() == 1 && saved[0].enabled {
                    item.expected = Some(saved[0].clone());
                    item.operation = Some(Operation::ReconnectChecked(saved[0].clone()));
                } else if !saved.is_empty() {
                    item.state = State::Skipped;
                    item.detail =
                        "Server list changed; choose the saved server from Servers.".into();
                    continue;
                }
            }
            let Some(operation) = item.operation.clone() else {
                item.state = State::Skipped;
                continue;
            };
            item.state = State::Running;
            self.active = Some(index);
            return Some(operation);
        }
        self.done = true;
        None
    }
    pub fn finish(&mut self, success: bool, message: String, screen: Option<Buffer>) {
        let Some(index) = self.active.take() else {
            return;
        };
        let item = &mut self.items[index];
        item.state = if success {
            State::Success
        } else {
            State::Failed
        };
        item.detail = message
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
        item.screen = if success {
            None
        } else {
            screen.filter(|s| s.area.width <= 160 && s.area.height <= 60)
        };
        if self.items.iter().filter(|i| i.screen.is_some()).count() > 8 {
            if let Some((_, old)) = self
                .items
                .iter_mut()
                .enumerate()
                .find(|(n, i)| *n != index && i.screen.is_some())
            {
                old.screen = None;
            }
        }
        self.done = self.items.iter().all(|i| i.state != State::Pending);
    }
    pub fn stop(&mut self) {
        for item in &mut self.items {
            if matches!(item.state, State::Pending | State::Running) {
                item.detail = if item.state == State::Running {
                    "Setup stopped; remote changes may already exist."
                } else {
                    "Not started."
                }
                .into();
                item.state = State::Stopped;
            }
        }
        self.active = None;
        self.done = true;
    }
    pub fn retry(&mut self) {
        for item in &mut self.items {
            if matches!(item.state, State::Failed | State::Stopped) && item.operation.is_some() {
                item.state = State::Pending;
                item.screen = None;
                item.detail.clear();
            }
        }
        self.active = None;
        self.done = !self.items.iter().any(|i| i.state == State::Pending);
    }
    pub fn status(&self, phase: &str) -> String {
        self.active.map_or_else(
            || phase.into(),
            |index| {
                format!(
                    "{}/{} · {} · {phase}",
                    index + 1,
                    self.items.len(),
                    self.items[index].device.name
                )
            },
        )
    }
    pub fn summary(&self) -> String {
        let ready = self
            .items
            .iter()
            .filter(|i| i.state == State::Success)
            .count();
        format!(
            "{ready}/{} ready · completed servers stay saved",
            self.items.len()
        )
    }
}

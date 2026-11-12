//! Client-owned machine dialogs. Rendering consumes cached state only.
use super::*;
use crate::client::machine_setup::{self, AddServer, Command, Event, Operation};
use crate::raw_input::RawInputEvent;
pub(in crate::client::shell) mod manager_sync;
mod tailscale_browser;
mod terminal_sessions;

use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

#[derive(Clone, Debug)]
pub(super) enum Page {
    Servers,
    Managers,
    DirectConfirm {
        action: crate::client::direct_sync::Action,
        text: String,
    },
    Add,
    Tailscale {
        join: bool,
    },
    Manage(SavedSshEndpoint),
    Imports(SavedSshEndpoint),
    Rename(SavedSshEndpoint),
    Sync,
    Join,
    Confirm {
        operation: Operation,
        text: String,
    },
    Approval(crate::remote::setup_progress::Approval),
    Running,
    Done,
}
#[derive(Debug)]
pub(super) struct Manager {
    pub id: u64,
    pub page: Page,
    pub fields: [TextEditor; 6],
    pub focus: usize,
    pub advanced: bool,
    pub setup_key: bool,
    pub suggestions: Vec<String>,
    pub tailscale: tailscale_browser::Browser,
    pub sync: String,
    pub direct: crate::client::direct_sync::View,
    pub error: Option<String>,
    pub screen: Option<Buffer>,
    pub cursor: Option<(u16, u16)>,
    pub success: bool,
    pub status: String,
    pub profiles: Vec<SavedSshEndpoint>,
    pub selected: usize,
    pub suggested: Option<usize>,
    pub last_operation: Option<Operation>,
    pub return_page: Option<Page>,
    pub import_request: u64,
    pub import_loading: bool,
    pub import_report: Option<crate::api::schema::TerminalSessionsReport>,
    pub import_workspace: Option<(SavedSshEndpoint, String)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hit {
    Hint,
    ManagerSync,
    RefreshSync,
    SyncStatusLine(usize),
    FindManagers,
    SyncManagers,
    PairManager(usize),
    ApprovePair(usize),
    DeclinePair(usize),
    ConfirmPair,
    CancelPair,
    ForgetManager(usize),
    StopManagers,
    BrowseTailscale,
    RefreshTailscale,
    TailscaleDevice(usize),
    Field(usize),
    Suggestion(usize),
    Server(usize),
    Add,
    Advanced,
    AutoKey,
    Connect,
    Back,
    Close,
    Rename,
    Reconnect,
    Update,
    SetupKey,
    Imports,
    RefreshImports,
    EnableImports,
    ImportTerminal(usize),
    OpenImported,
    Toggle,
    Remove,
    Sync,
    Host,
    Join,
    UseServer(usize),
    SyncNow,
    Leave,
    Confirm,
    Approve,
    Decline,
    Retry,
}
impl Manager {
    fn new(page: Page, profiles: Vec<SavedSshEndpoint>) -> Self {
        Self {
            id: machine_setup::next_id(),
            page,
            fields: std::array::from_fn(|_| TextEditor::default()),
            focus: 0,
            advanced: false,
            setup_key: true,
            suggestions: Vec::new(),
            tailscale: Default::default(),
            sync: "Checking shared-list status…".into(),
            direct: Default::default(),
            error: None,
            screen: None,
            cursor: None,
            success: false,
            status: String::new(),
            profiles,
            selected: 0,
            suggested: None,
            last_operation: None,
            return_page: None,
            import_request: 0,
            import_loading: false,
            import_report: None,
            import_workspace: None,
        }
    }
    fn form(&self) -> AddServer {
        AddServer {
            target: self.fields[0].to_string(),
            username: self.fields[5].to_string(),
            label: self.fields[1].to_string(),
            port: self.fields[2].to_string(),
            identity: self.fields[3].to_string(),
            session: self.fields[4].to_string(),
            setup_key: self.setup_key,
        }
    }
}
impl ClientShellState {
    pub(crate) fn receive_direct_sync(&mut self, view: crate::client::direct_sync::View) -> bool {
        let mut repaint = false;
        let pending_changed = self.direct_sync.incoming.len() != view.incoming.len();
        for incoming in &view.incoming {
            if !self
                .direct_sync
                .incoming
                .iter()
                .any(|p| p.request == incoming.request)
            {
                repaint|=self.push_endpoint_notice(ClientEndpointNoticeKind::Unavailable,format!("manager-pair-{}",incoming.request),"Manager pairing request",format!("{} requests server-list sharing. Open Servers → Server-list sync → Pair another manager. Compare code {}.",incoming.manager.name,incoming.code));
            }
        }
        self.direct_sync = view.clone();
        if let Some(ClientShellOverlay::Machines(manager)) = self.overlay.as_mut() {
            manager.direct = view;
            repaint |= matches!(manager.page, Page::Managers)
                || pending_changed && matches!(manager.page, Page::Sync | Page::Servers);
            if matches!(manager.page, Page::Managers) {
                manager.error = manager.direct.error.clone();
                manager.selected = manager
                    .selected
                    .min(menu_actions(manager).len().saturating_sub(1));
            }
        }
        repaint
    }
    pub(crate) fn receive_sync_status(&mut self, view: crate::client::sync_status::View) -> bool {
        let mut repaint = self.sync_view.badge != view.badge
            || self.sync_view.compact != view.compact
            || self.sync_view.level != view.level;
        if let Some(ClientShellOverlay::Machines(manager)) = self.overlay.as_mut() {
            if matches!(manager.page, Page::Sync) && manager.sync != view.details {
                let selected = menu_actions(manager).get(manager.selected).copied();
                manager.sync = view.details.clone();
                manager.selected = selected
                    .and_then(|hit| menu_actions(manager).iter().position(|h| *h == hit))
                    .unwrap_or(0);
                repaint = true;
            }
        }
        self.sync_view = view;
        repaint
    }
    pub(super) fn open_sync_status(&mut self, outcome: &mut ClientShellInput) {
        self.open_machine_page(Page::Sync, outcome);
    }
    pub(crate) fn open_machine_manager(&mut self, outcome: &mut ClientShellInput) {
        self.open_machine_page(Page::Servers, outcome);
    }
    pub(super) fn open_add_server(&mut self, outcome: &mut ClientShellInput) {
        self.open_machine_page(Page::Add, outcome);
    }
    pub(super) fn open_machine_imports(
        &mut self,
        id: &ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) {
        let ClientEndpointId::Ssh(id) = id else {
            return;
        };
        let Some(profile) = self
            .machine_profiles
            .iter()
            .find(|profile| &profile.id == id)
            .cloned()
        else {
            return;
        };
        self.open_machine_page(Page::Imports(profile), outcome);
        if let Some(ClientShellOverlay::Machines(manager)) = self.overlay.as_mut() {
            terminal_sessions::scan(manager, outcome);
        }
    }
    fn open_machine_page(&mut self, page: Page, outcome: &mut ClientShellInput) {
        let mut manager = Manager::new(page, self.machine_profiles.clone());
        manager.direct = self.direct_sync.clone();
        manager.sync = self.sync_view.details.clone();
        let id = manager.id;
        self.overlay = Some(ClientShellOverlay::Machines(Box::new(manager)));
        self.chrome_drag = None;
        outcome
            .actions
            .push(ClientShellAction::MachineManager(Command::Discover { id }));
        outcome.repaint = true;
    }
    pub(super) fn open_machine_context(
        &mut self,
        id: &ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) {
        let ClientEndpointId::Ssh(id) = id else {
            self.open_machine_manager(outcome);
            return;
        };
        if let Some(profile) = self.machine_profiles.iter().find(|p| &p.id == id).cloned() {
            self.open_machine_page(Page::Manage(profile), outcome);
        }
    }
    pub(super) fn open_machine_authentication(
        &mut self,
        id: &ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let ClientEndpointId::Ssh(id) = id else {
            return false;
        };
        let Some(profile) = self
            .machine_profiles
            .iter()
            .find(|profile| &profile.id == id)
            .cloned()
        else {
            return false;
        };
        self.open_machine_page(Page::Manage(profile), outcome);
        self.run_machine(Operation::SetupKey(id.to_string()), outcome);
        true
    }
    fn run_machine(&mut self, operation: Operation, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Machines(manager)) = self.overlay.as_mut() else {
            return;
        };
        manager.id = machine_setup::next_id();
        manager.tailscale.loading = false;
        manager.last_operation = Some(operation.clone());
        manager.import_workspace = None;
        if let Operation::ImportSession { profile, source } = operation {
            manager.import_request = machine_setup::next_id();
            manager.import_loading = true;
            manager.error = None;
            manager.page = Page::Imports(profile.clone());
            outcome
                .actions
                .push(ClientShellAction::MachineManager(Command::TerminalImport {
                    id: manager.id,
                    request: manager.import_request,
                    profile,
                    source,
                }));
            outcome.repaint = true;
            return;
        }
        manager.return_page = Some(manager.page.clone());
        manager.status = "Working…".into();
        let size = self.last_composed_size.unwrap_or((100, 30));
        let (width, height) = terminal_size(size.0, size.1);
        if width < 30 || height < 6 {
            manager.page = Page::Done;
            manager.success = false;
            manager.error = Some("Enlarge the window, then retry setup".into());
            outcome.repaint = true;
            return;
        }
        manager.page = Page::Running;
        manager.error = None;
        manager.screen = None;
        let (cols, rows) = self.last_composed_size.unwrap_or((100, 30));
        outcome
            .actions
            .push(ClientShellAction::MachineManager(Command::Run {
                id: manager.id,
                operation,
                cols: terminal_size(cols, rows).0,
                rows: terminal_size(cols, rows).1,
            }));
        outcome.repaint = true;
    }
    pub(crate) fn receive_machine_event(&mut self, event: Event) -> bool {
        let Some(ClientShellOverlay::Machines(manager)) = self.overlay.as_mut() else {
            return false;
        };
        match event {
            Event::TerminalScan {
                id,
                request,
                profile,
                result,
            } if id == manager.id
                && request == manager.import_request
                && matches!(&manager.page,Page::Imports(server) if server.id.as_str()==profile) =>
            {
                manager.import_loading = false;
                match result {
                    Ok(report) => manager.import_report = Some(report),
                    Err(error) => manager.error = Some(error),
                }
            }
            Event::TerminalImported {
                id,
                request,
                profile,
                result,
            } if id == manager.id
                && request == manager.import_request
                && matches!(&manager.page,Page::Imports(server) if server.id.as_str()==profile) =>
            {
                manager.import_loading = false;
                match result {
                    Ok(workspace) => {
                        if let Page::Imports(server) = &manager.page {
                            manager.import_workspace = Some((server.clone(), workspace));
                        }
                        manager.page = Page::Done;
                        manager.success = true;
                        manager.error=Some("Open the imported workspace to complete administrator authorization if requested. Closing this dialog leaves the import running.".into());
                    }
                    Err(error) => manager.error = Some(error),
                }
            }
            Event::DirectError { id, error } if id == manager.id => manager.error = Some(error),
            Event::Tailscale {
                id,
                request,
                result,
            } if id == manager.id && request == manager.tailscale.request => {
                let selected = manager
                    .tailscale
                    .matching_indices()
                    .get(manager.selected)
                    .and_then(|index| manager.tailscale.devices.get(*index))
                    .and_then(|device| device.target());
                manager.tailscale.loading = false;
                match result {
                    Ok(devices) => {
                        manager.tailscale.devices = devices
                            .into_iter()
                            .filter(|device| device.online == Some(true))
                            .collect();
                        manager.tailscale.error = None;
                        if matches!(manager.page, Page::Tailscale { .. }) {
                            manager.selected =
                                selected
                                    .and_then(|target| {
                                        manager.tailscale.matching_indices().iter().position(
                                            |index| {
                                                manager.tailscale.devices[*index].target().as_ref()
                                                    == Some(&target)
                                            },
                                        )
                                    })
                                    .unwrap_or(0);
                        }
                    }
                    Err(error) => manager.tailscale.error = Some(error),
                }
            }
            Event::Approval { id, prompt } if id == manager.id => {
                manager.page = Page::Approval(prompt);
                manager.selected = 0;
            }
            Event::Phase { id, phase } if id == manager.id => {
                manager.status = phase.label().into();
                if matches!(manager.page, Page::Approval(_)) {
                    manager.page = Page::Running;
                }
            }
            Event::Status { id, text } if id == manager.id => manager.sync = text,
            Event::Suggestions { id, targets, sync } if id == manager.id => {
                manager.suggestions = targets;
                manager.sync = sync;
            }
            Event::Screen { id, buffer, cursor } if id == manager.id => {
                manager.screen = Some(buffer);
                manager.cursor = cursor;
            }
            Event::Finished {
                id,
                success,
                message,
            } if id == manager.id => {
                manager.page = Page::Done;
                manager.success = success;
                manager.error = Some(message);
                let retry = if success {
                    match &manager.last_operation {
                        Some(
                            Operation::SetupKey(id)
                            | Operation::Reconnect(id)
                            | Operation::Update(id),
                        ) => manager
                            .profiles
                            .iter()
                            .find(|profile| profile.id.as_str() == id)
                            .map(|profile| profile.id.clone()),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(id) = retry {
                    self.retry_machine(&ClientEndpointId::Ssh(id));
                }
            }
            _ => return false,
        }
        true
    }
    pub(super) fn machine_action(&mut self, hit: Hit, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Machines(mut manager)) = self.overlay.take() else {
            return;
        };
        let old_page = std::mem::discriminant(&manager.page);
        let mut operation = None;
        match hit {
            Hit::Imports=>{
                let profile=match &manager.page {Page::Manage(profile)=>Some(profile.clone()),Page::Done=>match &manager.last_operation {Some(Operation::EnableImports(profile))=>Some(profile.clone()),_=>None},_=>None};
                if let Some(profile)=profile{manager.page=Page::Imports(profile);manager.import_report=None;manager.import_workspace=None;terminal_sessions::scan(&mut manager,outcome);}
            },
            Hit::RefreshImports=>terminal_sessions::scan(&mut manager,outcome),
            Hit::EnableImports=>if let Page::Imports(profile)=&manager.page {
                manager.page=Page::Confirm{operation:Operation::EnableImports(profile.clone()),text:format!("Enable live terminal import on {}? This installs the system reptyr package and a root-owned Herdr helper. Administrator authorization may be requested. Existing terminals are not moved until you select one; kernel security settings are not changed.",profile.label)};
            },
            Hit::ImportTerminal(index)=>if let Page::Imports(profile)=&manager.page {
                if let Some(source)=manager.import_report.as_ref().filter(|report|report.helper_ready).and_then(|report|report.sessions.get(index)).cloned() {
                    manager.page=Page::Confirm{operation:Operation::ImportSession{profile:profile.clone(),source:source.identity.clone()},text:format!("Move {} ({}, {} processes) on {} into a new Herdr workspace without restarting its programs? The whole terminal moves and its original window loses control. Existing scrollback and agent registration are not imported. Administrator authorization may be requested in the new workspace. Keep that workspace open while the transfer runs. Source PID: {}.",source.terminal,source.command,source.process_count,profile.label,source.identity.pid)};
                }
            },
            Hit::OpenImported=>if let Some((profile,workspace))=&manager.import_workspace {
                if self.machine_profiles.iter().any(|current|current.id==profile.id && current.target==profile.target && current.session==profile.session && current.enabled) {
                    outcome.actions.push(ClientShellAction::ActivateEndpoint{endpoint_id:ClientEndpointId::Ssh(profile.id.clone()),target:Some(ClientEndpointFocusTarget::Workspace(workspace.clone()))});
                    self.overlay=None;outcome.repaint=true;return;
                } else {manager.error=Some("Server entry changed; open the import workspace on the original server.".into());}
            },
            Hit::Hint => {},
            Hit::ManagerSync => {manager.page=Page::Managers;manager.error=manager.direct.error.clone();outcome.actions.push(ClientShellAction::MachineManager(Command::Direct {id:manager.id,action:crate::client::direct_sync::Action::Discover}));},
            Hit::FindManagers|Hit::SyncManagers => {let action=if hit==Hit::FindManagers{crate::client::direct_sync::Action::Discover}else{crate::client::direct_sync::Action::SyncNow};outcome.actions.push(ClientShellAction::MachineManager(Command::Direct{id:manager.id,action}));},
            Hit::PairManager(_)|Hit::ApprovePair(_)|Hit::ConfirmPair|Hit::ForgetManager(_)|Hit::StopManagers => if let Some((action,text))=manager_sync::consent(&manager,hit){manager.page=Page::DirectConfirm{action,text};},
            Hit::DeclinePair(index)=>if let Some(p)=manager.direct.incoming.get(index){outcome.actions.push(ClientShellAction::MachineManager(Command::Direct{id:manager.id,action:crate::client::direct_sync::Action::Approve{request:p.request.clone(),accepted:false}}));},
            Hit::CancelPair=>if let Some(p)=&manager.direct.outgoing{outcome.actions.push(ClientShellAction::MachineManager(Command::Direct{id:manager.id,action:crate::client::direct_sync::Action::Cancel{request:p.request.clone()}}));},
            Hit::BrowseTailscale => if matches!(manager.page, Page::Add | Page::Join) {
                manager.page=Page::Tailscale {join:matches!(manager.page,Page::Join)};
                if let Some(action)=manager.tailscale.refresh(manager.id) {outcome.actions.push(action);}
            },
            Hit::RefreshTailscale => if matches!(manager.page,Page::Tailscale{..}) {
                if let Some(action)=manager.tailscale.refresh(manager.id) {outcome.actions.push(action);}
            },
            Hit::TailscaleDevice(index) => if let Page::Tailscale{join}=manager.page {
                if !manager.tailscale.matching_indices().contains(&index) {self.overlay=Some(ClientShellOverlay::Machines(manager));return;}
                if let Some(target)=manager.tailscale.devices.get(index).and_then(|device|device.target()) {
                    let prior_user=manager.fields[0].trim().strip_prefix("ssh://").unwrap_or(manager.fields[0].trim()).rsplit_once('@').map(|(user,_)|user.to_owned()).filter(|user|!user.contains(':'));
                    if join { manager.fields[0]=prior_user.map_or(target.clone(),|user|format!("{user}@{target}")).as_str().into(); }
                    else { manager.fields[0]=target.as_str().into();if manager.fields[5].is_empty(){if let Some(user)=prior_user{manager.fields[5]=user.as_str().into();}} }
                    manager.page=if join {Page::Join}else{Page::Add};
                    manager.focus=if join {0}else{5};manager.error=None;
                }
            },
            Hit::Close => {
                outcome.actions.push(ClientShellAction::MachineManager(Command::Cancel { id: manager.id }));
                outcome.repaint = true;
                return;
            }
            Hit::Back => {
                manager.tailscale.loading=false;
                outcome.actions.push(ClientShellAction::MachineManager(Command::Cancel { id: manager.id }));
                manager.id = machine_setup::next_id();
                manager.page = if matches!(manager.page,Page::DirectConfirm{..}) {Page::Managers}
                else if let Page::Tailscale { join } = manager.page { if join { Page::Join } else { Page::Add } }
                else if matches!(manager.page, Page::Running | Page::Approval(_))
                    || matches!(manager.page, Page::Done) && !manager.success {
                    manager.return_page.take().unwrap_or(Page::Servers)
                } else { Page::Servers };
                manager.profiles = self.machine_profiles.clone();
            }
            Hit::Add => {
                manager.page = Page::Add;
                manager.focus = 0;
                manager.fields = std::array::from_fn(|_| TextEditor::default());
                manager.advanced = false;
                manager.error = None;
            }
            Hit::Advanced => manager.advanced = !manager.advanced,
            Hit::AutoKey => manager.setup_key = !manager.setup_key,
            Hit::Field(n) => if visible_fields(&manager).contains(&n) { manager.focus = n; },
            Hit::Suggestion(n) => if let Some(target) = manager.suggestions.get(n) {
                manager.fields[0] = target.as_str().into();
                manager.focus = 5;
            },
            Hit::Server(n) => if let Some(profile) = manager.profiles.get(n) {
                manager.page = Page::Manage(profile.clone());
            },
            Hit::Connect => match &manager.page {
                Page::Add => match manager.form().destination() {
                    Ok(_) => operation = Some(Operation::Add(manager.form())),
                    Err(error) => manager.error = Some(error),
                },
                Page::Rename(profile) => {
                    if manager.fields[1].trim().is_empty() {
                        manager.error = Some("Enter a server name".into());
                    } else {
                        operation = Some(Operation::Rename { id: profile.id.to_string(), name: manager.fields[1].trim().into() });
                    }
                }
                Page::Join => {
                    let target = manager.fields[0].trim();
                    if target.is_empty() { manager.error = Some("Choose an SSH address for the shared list".into()); }
                    else { manager.page = Page::Confirm { operation: Operation::SyncUse(target.into()), text: format!("Use {target} as the shared-list host? Existing server lists will merge.") }; }
                }
                _ => {}
            },
            Hit::Rename => if let Page::Manage(profile) = &manager.page {
                manager.fields[1] = profile.label.as_str().into();
                manager.page = Page::Rename(profile.clone());
                manager.focus = 1;
            },
            Hit::Reconnect => if let Page::Manage(profile) = &manager.page {
                operation = Some(Operation::Reconnect(profile.id.to_string()));
            },
            Hit::Update => if let Page::Manage(profile) = &manager.page {
                operation = Some(Operation::Update(profile.id.to_string()));
            },
            Hit::SetupKey => if let Page::Manage(profile) = &manager.page {
                manager.page = Page::Confirm {
                    operation: Operation::SetupKey(profile.id.to_string()),
                    text: format!("Set up password-free SSH for {} ({})? Herdr creates a dedicated Ed25519 key without a passphrase, stored privately on this computer. It adds only the public key to this account's authorized_keys, preserves existing keys, and verifies a fresh login before saving the setting. Enter the current SSH password once if prompted. Private keys are never synced.", profile.label, profile.target),
                };
            },
            Hit::Toggle => if let Page::Manage(profile) = &manager.page {
                manager.page = Page::Confirm {
                    operation: Operation::Enable { id: profile.id.to_string(), enabled: !profile.enabled },
                    text: format!("{} {} in the server list? This also applies to other computers when sharing is enabled. Running sessions stay running.", if profile.enabled { "Disable" } else { "Enable" }, profile.label),
                };
            },
            Hit::Remove => if let Page::Manage(profile) = &manager.page {
                manager.page = Page::Confirm {
                    operation: Operation::Remove(profile.id.to_string()),
                    text: format!("Remove {} from the server list? This also removes the shared entry when sharing is enabled. Running sessions stay running.", profile.label),
                };
            },
            Hit::RefreshSync => outcome.actions.push(ClientShellAction::MachineManager(Command::RefreshStatus { id: manager.id })),
            Hit::SyncStatusLine(_) => {},
            Hit::Sync => {
                manager.page = Page::Sync;
                outcome.actions.push(ClientShellAction::MachineManager(Command::Status { id: manager.id }));
            },
            Hit::Host => manager.page = Page::Confirm {
                operation: Operation::SyncHost,
                text: "Host the shared server list on this computer? Other computers can join using your SSH address. SSH access must be enabled.".into(),
            },
            Hit::Join => { manager.page = Page::Join; manager.focus = 0; manager.fields[0].clear(); }
            Hit::UseServer(n) => if let Some(profile) = manager.profiles.get(n) {
                manager.page = Page::Confirm { operation: Operation::SyncUse(profile.target.clone()), text: format!("Use {} as the shared-list host? Existing server lists will merge.", profile.label) };
            },
            Hit::SyncNow => operation = Some(Operation::SyncNow),
            Hit::Leave => manager.page = Page::Confirm {
                operation: Operation::SyncLeave,
                text: "Stop sharing on this computer? Your local server list and remote sessions will stay available.".into(),
            },
            Hit::Approve | Hit::Decline => if let Page::Approval(prompt) = &manager.page {
                if confirmation_fits(&prompt.detail, self.last_composed_size.unwrap_or((100, 30))) {
                    outcome.actions.push(ClientShellAction::MachineManager(Command::Approval { id: manager.id, request: prompt.request, accepted: hit == Hit::Approve }));
                    manager.page = Page::Running;
                } else { manager.error = Some("Enlarge the window to review the full request".into()); }
            },
            Hit::Confirm => if let Page::DirectConfirm{action,text}=&manager.page {
                if confirmation_fits(text,self.last_composed_size.unwrap_or((100,30))){outcome.actions.push(ClientShellAction::MachineManager(Command::Direct{id:manager.id,action:action.clone()}));manager.page=Page::Managers;manager.error=None;}else{manager.error=Some("Enlarge the window to review the full pairing request".into());}
            } else if let Page::Confirm { operation: op, text } = &manager.page {
                if confirmation_fits(text, self.last_composed_size.unwrap_or((100, 30))) { operation = Some(op.clone()); }
                else { manager.error = Some("Enlarge the window to review the full confirmation".into()); }
            },
            Hit::Retry => operation = manager.last_operation.clone(),
        }
        if std::mem::discriminant(&manager.page) != old_page {
            manager.selected = 0;
            manager.suggested = None;
        }
        self.overlay = Some(ClientShellOverlay::Machines(manager));
        if let Some(operation) = operation {
            self.run_machine(operation, outcome);
        }
        outcome.repaint = true;
    }
    pub(super) fn handle_machine_raw(
        &mut self,
        event: &RawInputEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::Machines(_))) {
            return false;
        }
        if let RawInputEvent::Key(key) = event {
            if self.handle_modal_paste_shortcut_with(
                key,
                outcome,
                crate::platform::read_clipboard_text,
            ) {
                return true;
            }
        }
        let Some(ClientShellOverlay::Machines(manager)) = self.overlay.as_mut() else {
            return false;
        };
        match event {
            RawInputEvent::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return true;
                }
                if matches!(manager.page, Page::Running | Page::Approval(_))
                    && key.code == KeyCode::Char('g')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.machine_action(Hit::Back, outcome);
                    return true;
                }
                if matches!(manager.page, Page::Running) {
                    if key.code == KeyCode::Char('g')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.machine_action(Hit::Back, outcome);
                    } else if let Some(bytes) = key_bytes(key) {
                        outcome
                            .actions
                            .push(ClientShellAction::MachineManager(Command::Input {
                                id: manager.id,
                                bytes,
                            }));
                    }
                    return true;
                }
                if key.code == KeyCode::Char('k')
                    && key.modifiers == KeyModifiers::CONTROL
                    && matches!(manager.page, Page::Add)
                {
                    self.machine_action(Hit::AutoKey, outcome);
                    return true;
                }
                if key.code == KeyCode::Char('t')
                    && matches!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && matches!(manager.page, Page::Add | Page::Join)
                {
                    self.machine_action(Hit::BrowseTailscale, outcome);
                    return true;
                }
                if key.code == KeyCode::Char('r')
                    && key.modifiers == KeyModifiers::CONTROL
                    && matches!(manager.page, Page::Imports(_))
                {
                    self.machine_action(Hit::RefreshImports, outcome);
                    return true;
                }
                if key.code == KeyCode::Char('r')
                    && key.modifiers == KeyModifiers::CONTROL
                    && matches!(manager.page, Page::Tailscale { .. })
                {
                    self.machine_action(Hit::RefreshTailscale, outcome);
                    return true;
                }
                if key.code == KeyCode::Esc {
                    let hit = if matches!(manager.page, Page::Tailscale { .. }) {
                        Hit::Back
                    } else {
                        Hit::Close
                    };
                    self.machine_action(hit, outcome);
                    return true;
                }
                if matches!(manager.page, Page::Tailscale { .. }) {
                    if let Some(changed) = manager.tailscale.query.handle_key(key) {
                        if changed {
                            manager.selected = 0;
                        }
                        outcome.repaint = true;
                        return true;
                    }
                }
                let field = editable(manager);
                if let Some(field) = field {
                    match key.code {
                        KeyCode::Tab | KeyCode::BackTab => {
                            let fields = visible_fields(manager);
                            let index = fields.iter().position(|f| *f == field).unwrap_or(0);
                            manager.focus = fields[(index
                                + if key.code == KeyCode::BackTab {
                                    fields.len() - 1
                                } else {
                                    1
                                })
                                % fields.len()];
                        }
                        KeyCode::Char('a')
                            if key.modifiers == KeyModifiers::ALT
                                && matches!(manager.page, Page::Add) =>
                        {
                            manager.advanced = !manager.advanced;
                        }
                        KeyCode::Up | KeyCode::Down if field == 0 => {
                            let options = filtered_suggestions(manager);
                            let current =
                                options.iter().position(|n| Some(*n) == manager.suggested);
                            manager.suggested = if options.is_empty() {
                                None
                            } else {
                                Some(
                                    options[if key.code == KeyCode::Up {
                                        current.unwrap_or(0).saturating_sub(1)
                                    } else {
                                        current.map_or(0, |n| (n + 1).min(options.len() - 1))
                                    }],
                                )
                            };
                        }
                        KeyCode::Enter => {
                            let hit = manager
                                .suggested
                                .filter(|_| field == 0)
                                .map(Hit::Suggestion)
                                .unwrap_or(Hit::Connect);
                            self.machine_action(hit, outcome);
                            return true;
                        }
                        _ => {
                            manager.fields[field].handle_key(key);
                            manager.suggested = None;
                            manager.error = None;
                        }
                    }
                } else {
                    let options = menu_actions(manager);
                    match key.code {
                        KeyCode::Up | KeyCode::BackTab => {
                            manager.selected = manager.selected.saturating_sub(1)
                        }
                        KeyCode::Down | KeyCode::Tab => {
                            manager.selected =
                                (manager.selected + 1).min(options.len().saturating_sub(1))
                        }
                        KeyCode::Enter => {
                            if let Some(hit) = options.get(manager.selected).copied() {
                                self.machine_action(hit, outcome);
                            }
                            return true;
                        }
                        KeyCode::Char('a') if matches!(manager.page, Page::Servers) => {
                            self.machine_action(Hit::Add, outcome);
                            return true;
                        }
                        KeyCode::Char('r') if matches!(manager.page, Page::Sync) => {
                            self.machine_action(Hit::RefreshSync, outcome);
                            return true;
                        }
                        KeyCode::Char('r')
                            if matches!(manager.page, Page::Done) && !manager.success =>
                        {
                            self.machine_action(Hit::Retry, outcome);
                            return true;
                        }
                        _ => {}
                    }
                }
                outcome.repaint = true;
                true
            }
            RawInputEvent::Paste(text) => {
                if matches!(manager.page, Page::Tailscale { .. }) {
                    manager.tailscale.query.insert(text);
                    manager.selected = 0;
                    outcome.repaint = true;
                } else if matches!(manager.page, Page::Running) {
                    if text.len() > 64 * 1024 {
                        manager.error = Some("Paste is too large for the setup terminal".into());
                        outcome.repaint = true;
                    } else {
                        outcome
                            .actions
                            .push(ClientShellAction::MachineManager(Command::Paste {
                                id: manager.id,
                                text: text.clone(),
                            }));
                    }
                } else if let Some(field) = editable(manager) {
                    manager.fields[field].insert(text);
                    manager.suggested = None;
                    manager.error = None;
                    outcome.repaint = true;
                }
                true
            }
            RawInputEvent::Text(text) => {
                if matches!(manager.page, Page::Tailscale { .. }) {
                    manager.tailscale.query.insert(text.as_str());
                    manager.selected = 0;
                    outcome.repaint = true;
                } else if matches!(manager.page, Page::Running) {
                    outcome
                        .actions
                        .push(ClientShellAction::MachineManager(Command::Input {
                            id: manager.id,
                            bytes: text.as_str().as_bytes().to_vec(),
                        }));
                } else if let Some(field) = editable(manager) {
                    manager.fields[field].insert(text.as_str());
                    manager.suggested = None;
                    manager.error = None;
                    outcome.repaint = true;
                }
                true
            }
            RawInputEvent::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let point = (mouse.column, mouse.row);
                        if let Some(hit) = self
                            .hits
                            .machine_actions
                            .iter()
                            .find(|(r, _)| super::contains(*r, point))
                            .map(|(_, h)| *h)
                        {
                            self.machine_action(hit, outcome);
                        }
                    }
                    MouseEventKind::ScrollDown
                        if matches!(manager.page, Page::Add) && manager.advanced =>
                    {
                        let fields = visible_fields(manager);
                        let index = fields.iter().position(|f| *f == manager.focus).unwrap_or(0);
                        manager.focus = fields[(index + 1).min(fields.len() - 1)];
                        outcome.repaint = true;
                    }
                    MouseEventKind::ScrollUp
                        if matches!(manager.page, Page::Add) && manager.advanced =>
                    {
                        let fields = visible_fields(manager);
                        let index = fields.iter().position(|f| *f == manager.focus).unwrap_or(0);
                        manager.focus = fields[index.saturating_sub(1)];
                        outcome.repaint = true;
                    }
                    MouseEventKind::ScrollDown => {
                        manager.selected = (manager.selected + 3)
                            .min(menu_actions(manager).len().saturating_sub(1));
                        outcome.repaint = true;
                    }
                    MouseEventKind::ScrollUp => {
                        manager.selected = manager.selected.saturating_sub(3);
                        outcome.repaint = true;
                    }
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }
}
fn visible_fields(manager: &Manager) -> &'static [usize] {
    match manager.page {
        Page::Add if manager.advanced => &[0, 5, 1, 2, 3, 4],
        Page::Add => &[0, 5, 1],
        Page::Join => &[0],
        Page::Rename(_) => &[1],
        _ => &[],
    }
}
fn editable(manager: &Manager) -> Option<usize> {
    match manager.page {
        Page::Add => Some(if visible_fields(manager).contains(&manager.focus) {
            manager.focus
        } else {
            0
        }),
        Page::Join => Some(0),
        Page::Rename(_) => Some(1),
        _ => None,
    }
}
fn filtered_suggestions(manager: &Manager) -> Vec<usize> {
    let query = manager.fields[0].to_lowercase();
    manager
        .suggestions
        .iter()
        .enumerate()
        .filter(|(_, s)| query.is_empty() || s.to_lowercase().contains(&query))
        .map(|(n, _)| n)
        .collect()
}
pub(super) fn menu_actions(manager: &Manager) -> Vec<Hit> {
    match &manager.page {
        Page::Managers => manager_sync::rows(&manager.direct)
            .into_iter()
            .filter_map(|(_, hit)| (hit != Hit::Hint).then_some(hit))
            .collect(),
        Page::DirectConfirm { .. } => vec![Hit::Back, Hit::Confirm],
        Page::Tailscale { .. } => manager
            .tailscale
            .matching_indices()
            .into_iter()
            .map(Hit::TailscaleDevice)
            .chain(std::iter::once(Hit::RefreshTailscale))
            .collect(),
        Page::Servers => [
            vec![Hit::Add, Hit::Sync],
            (0..manager.profiles.len()).map(Hit::Server).collect(),
        ]
        .concat(),
        Page::Manage(_) => vec![
            Hit::Reconnect,
            Hit::Update,
            Hit::SetupKey,
            Hit::Imports,
            Hit::Rename,
            Hit::Toggle,
            Hit::Remove,
            Hit::Sync,
        ],
        Page::Sync => [
            vec![Hit::ManagerSync],
            (0..manager.sync.lines().count())
                .map(Hit::SyncStatusLine)
                .collect(),
            vec![Hit::RefreshSync, Hit::Host, Hit::Join],
            (0..manager.profiles.len()).map(Hit::UseServer).collect(),
            vec![Hit::SyncNow, Hit::Leave],
        ]
        .concat(),
        Page::Confirm { .. } => vec![Hit::Confirm, Hit::Back],
        Page::Approval(_) => vec![Hit::Decline, Hit::Approve, Hit::Back],
        Page::Done if !manager.success => vec![Hit::Retry, Hit::Back, Hit::Close],
        Page::Done if manager.import_workspace.is_some() => {
            vec![Hit::OpenImported, Hit::Back, Hit::Close]
        }
        Page::Done if matches!(manager.last_operation, Some(Operation::EnableImports(_))) => {
            vec![Hit::Imports, Hit::Back, Hit::Close]
        }
        Page::Done => vec![Hit::Back, Hit::Close],
        Page::Imports(_) => terminal_sessions::rows(manager)
            .into_iter()
            .filter_map(|(_, hit)| (hit != Hit::Hint).then_some(hit))
            .collect(),
        _ => Vec::new(),
    }
}
fn key_bytes(key: &crate::input::TerminalKey) -> Option<Vec<u8>> {
    let mut bytes = match key.code {
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![27],
        KeyCode::Backspace => vec![127],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii() => {
            vec![(c.to_ascii_uppercase() as u8) & 31]
        }
        KeyCode::Char(c) => key
            .generated_text
            .clone()
            .unwrap_or_else(|| c.to_string())
            .into_bytes(),
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 27);
    }
    Some(bytes)
}

pub(super) fn render(
    buffer: &mut Buffer,
    manager: &Manager,
    palette: &Palette,
) -> Option<super::render::overlays::OverlayRender> {
    use super::render::overlays::{panel, popup};
    use super::render::put_text;
    let Some(area) = popup(buffer.area, 96, 32) else {
        return Some(small_window(buffer, palette));
    };
    let inner = panel(buffer, area, palette.accent, palette.panel_bg)?;
    if inner.width < 20 || inner.height < 8 {
        return Some(small_window(buffer, palette));
    }
    let mut hits = Vec::new();
    let normal = Style::default().fg(palette.text).bg(palette.panel_bg);
    let accent = normal.fg(palette.accent);
    let title = match &manager.page {
        Page::Managers => "Direct manager sync",
        Page::DirectConfirm { .. } => "Confirm manager pairing",
        Page::Servers => "Servers",
        Page::Add => "Add server",
        Page::Tailscale { .. } => "Tailscale devices",
        Page::Manage(p) => &p.label,
        Page::Imports(_) => "Import running terminal sessions",
        Page::Rename(_) => "Rename server",
        Page::Sync => "Server-list sync",
        Page::Join => "Join a shared server list",
        Page::Confirm { .. } => "Confirm",
        Page::Approval(prompt) => &prompt.title,
        Page::Running => "Connect and set up server",
        Page::Done => {
            if manager.success {
                "Ready"
            } else {
                "Setup needs attention"
            }
        }
    };
    put_text(
        buffer,
        inner.x,
        inner.y,
        inner.width,
        title,
        accent.add_modifier(Modifier::BOLD),
    );
    let mut y = inner.y.saturating_add(2);
    let bottom = inner.bottom().saturating_sub(3);
    let chosen = menu_actions(manager).get(manager.selected).copied();
    let visible = usize::from(bottom.saturating_sub(y));
    let scroll = if editable(manager).is_none() {
        manager
            .selected
            .saturating_add(match manager.page {
                Page::Manage(_) | Page::Managers => 1,
                Page::Imports(_) => {
                    if manager
                        .import_report
                        .as_ref()
                        .is_some_and(|report| report.helper_ready)
                    {
                        2
                    } else {
                        1
                    }
                }

                _ => 0,
            })
            .saturating_sub(visible.saturating_sub(1))
    } else {
        visible_fields(manager)
            .iter()
            .position(|f| Some(*f) == editable(manager))
            .unwrap_or(0)
            .saturating_sub(visible.saturating_sub(1))
    };
    let mut row_index = 0usize;
    {
        let mut row = |label: &str, hit: Hit, selected: bool| {
            row_index += 1;
            if row_index <= scroll {
                return;
            }
            let selected = selected || chosen == Some(hit);
            if y < bottom {
                let rect = Rect::new(inner.x, y, inner.width, 1);
                put_text(
                    buffer,
                    rect.x,
                    rect.y,
                    rect.width,
                    label,
                    if selected {
                        normal.bg(palette.active_row_bg)
                    } else {
                        normal
                    },
                );
                hits.push((rect, hit));
                y += 1;
            }
        };
        match &manager.page {
            Page::Managers => {
                for (text, hit) in manager_sync::rows(&manager.direct) {
                    row(&text, hit, false);
                }
            }
            Page::Servers => {
                row("＋ Add server", Hit::Add, false);
                let sync_label = if manager.direct.incoming.is_empty() {
                    "⇄ Server-list sync".into()
                } else {
                    format!(
                        "⇄ Server-list sync · {} pairing requests",
                        manager.direct.incoming.len()
                    )
                };
                row(&sync_label, Hit::Sync, false);
                for (n, p) in manager.profiles.iter().enumerate() {
                    row(
                        &format!("{}  {}", if p.enabled { "●" } else { "○" }, p.label),
                        Hit::Server(n),
                        false,
                    );
                }
            }
            Page::Manage(p) => {
                row(&p.target, Hit::Field(0), false);
                row("Reconnect / authenticate", Hit::Reconnect, false);
                row("Update Rust build…", Hit::Update, false);
                row("Set up SSH key (no more passwords)…", Hit::SetupKey, false);
                row("Discover / import running terminals…", Hit::Imports, false);
                row("Rename", Hit::Rename, false);
                row(
                    if p.enabled { "Disable…" } else { "Enable…" },
                    Hit::Toggle,
                    false,
                );
                row("Remove from server list…", Hit::Remove, false);
                row("Server-list sync", Hit::Sync, false);
            }
            Page::Imports(profile) => {
                row(&profile.target, Hit::Hint, false);
                for (text, hit) in terminal_sessions::rows(manager) {
                    row(&text, hit, false);
                }
            }
            Page::Add | Page::Join | Page::Rename(_) => {
                let fields: Vec<(usize, &str)> = if matches!(manager.page, Page::Rename(_)) {
                    vec![(1, "Name")]
                } else if matches!(manager.page, Page::Join) {
                    vec![(0, "SSH address")]
                } else if manager.advanced {
                    vec![
                        (0, "Host / IP"),
                        (5, "Username"),
                        (1, "Name"),
                        (2, "Port"),
                        (3, "Key file (local)"),
                        (4, "Session (auto)"),
                    ]
                } else {
                    vec![(0, "Host / IP"), (5, "Username"), (1, "Name")]
                };
                let field_count = fields.len();
                for (n, label) in fields {
                    row(
                        &format!(
                            "{label}: {}{}",
                            manager.fields[n]
                                .viewport(inner.width.saturating_sub(label.len() as u16 + 4))
                                .0,
                            if manager.focus == n { "▏" } else { "" }
                        ),
                        Hit::Field(n),
                        manager.focus == n,
                    );
                }
                if matches!(manager.page, Page::Add) {
                    row(
                        if manager.advanced {
                            "▾ Advanced"
                        } else {
                            "▸ Advanced"
                        },
                        Hit::Advanced,
                        false,
                    );
                    row(
                        if manager.setup_key {
                            "☑ Automatic SSH key (Linux/macOS) · Ctrl+K"
                        } else {
                            "☐ Automatic SSH key (Linux/macOS) · Ctrl+K"
                        },
                        Hit::AutoKey,
                        false,
                    );
                }
                if matches!(manager.page, Page::Add | Page::Join) {
                    row(
                        if matches!(manager.page, Page::Add) && manager.setup_key {
                            "Enter your SSH password once; future logins use a dedicated key."
                        } else {
                            "Uses your SSH key or agent; prompts for a password if needed."
                        },
                        Hit::Hint,
                        false,
                    );
                    row(
                        "Password prompts appear here after Connect (input is hidden).",
                        Hit::Hint,
                        false,
                    );
                    if matches!(manager.page, Page::Add) {
                        if manager.setup_key {
                            row("Key has no passphrase, stays private here; only its public key is copied.",Hit::Hint,false);
                        }
                        row(
                            "Username blank: use SSH config or your local username.",
                            Hit::Hint,
                            false,
                        );
                    }
                }
                let hint_rows = if matches!(manager.page, Page::Add) {
                    3 + usize::from(manager.setup_key)
                } else if matches!(manager.page, Page::Join) {
                    2
                } else {
                    0
                };
                let prefix_rows =
                    field_count + 2 * usize::from(matches!(manager.page, Page::Add)) + hint_rows;
                let suggestion_rows = visible
                    .saturating_sub(prefix_rows.saturating_sub(scroll))
                    .min(6);
                let options = filtered_suggestions(manager);
                let start = options
                    .iter()
                    .position(|n| Some(*n) == manager.suggested)
                    .unwrap_or(0)
                    .saturating_sub(suggestion_rows.saturating_sub(1));
                for n in options.into_iter().skip(start).take(suggestion_rows) {
                    row(
                        &format!("  {}", manager.suggestions[n]),
                        Hit::Suggestion(n),
                        manager.suggested == Some(n),
                    );
                }
            }
            Page::Sync => {
                row(
                    "Pair another manager over Tailscale…",
                    Hit::ManagerSync,
                    false,
                );
                for (n, line) in manager.sync.lines().enumerate() {
                    row(line, Hit::SyncStatusLine(n), false);
                }
                row("Refresh manager status (Ctrl+R)", Hit::RefreshSync, false);
                row("Host shared list on this computer…", Hit::Host, false);
                row("Join using an SSH address…", Hit::Join, false);
                for (n, p) in manager.profiles.iter().enumerate() {
                    row(
                        &format!("Use {} as shared-list host…", p.label),
                        Hit::UseServer(n),
                        false,
                    );
                }
                row("Sync now", Hit::SyncNow, false);
                row("Leave shared list…", Hit::Leave, false);
            }
            Page::Confirm { .. }
            | Page::DirectConfirm { .. }
            | Page::Approval(_)
            | Page::Tailscale { .. } => {}
            Page::Done if !manager.success => {}
            Page::Running | Page::Done => {}
        }
    }
    let browser_cursor = if matches!(manager.page, Page::Tailscale { .. }) {
        tailscale_browser::render(buffer, inner, manager, palette, &mut hits)
    } else {
        None
    };
    if matches!(
        manager.page,
        Page::Confirm { .. } | Page::DirectConfirm { .. }
    ) {
        if let Page::Confirm { text, .. } | Page::DirectConfirm { text, .. } = &manager.page {
            use ratatui::widgets::{Paragraph, Widget, Wrap};
            Paragraph::new(text.as_str())
                .wrap(Wrap { trim: false })
                .style(normal)
                .render(
                    Rect::new(
                        inner.x,
                        inner.y + 2,
                        inner.width,
                        inner.height.saturating_sub(6),
                    ),
                    buffer,
                );
        }
    }
    if let Page::Approval(prompt) = &manager.page {
        use ratatui::widgets::{Paragraph, Widget, Wrap};
        Paragraph::new(prompt.detail.as_str())
            .wrap(Wrap { trim: false })
            .style(normal)
            .render(
                Rect::new(
                    inner.x,
                    inner.y + 2,
                    inner.width,
                    inner.height.saturating_sub(6),
                ),
                buffer,
            );
        let no = Rect::new(inner.x, inner.bottom() - 2, 12, 1);
        put_text(
            buffer,
            no.x,
            no.y,
            no.width,
            if manager.selected == 0 {
                "> Decline"
            } else {
                "  Decline"
            },
            accent,
        );
        hits.push((no, Hit::Decline));
        let yes = Rect::new(
            inner.x + 14,
            inner.bottom() - 2,
            inner.width.saturating_sub(14),
            1,
        );
        put_text(
            buffer,
            yes.x,
            yes.y,
            yes.width,
            match (prompt.restart, manager.selected == 1) {
                (true, true) => "> Approve restart",
                (true, false) => "  Approve restart",
                (false, true) => "> Install",
                (false, false) => "  Install",
            },
            accent,
        );
        hits.push((yes, Hit::Approve));
    }
    if matches!(manager.page, Page::Running) {
        put_text(
            buffer,
            inner.x,
            inner.y + 1,
            inner.width,
            &manager.status,
            accent,
        );
    }
    let mut cursor = browser_cursor;
    if let Some(field) = editable(manager) {
        if let Some((rect, _)) = hits.iter().find(|(_, hit)| *hit == Hit::Field(field)) {
            let label = match field {
                0 => {
                    if matches!(manager.page, Page::Join) {
                        "SSH address"
                    } else {
                        "Host / IP"
                    }
                }
                1 => "Name",
                2 => "Port",
                3 => "Key file (local)",
                5 => "Username",
                _ => "Session (auto)",
            };
            let prefix = label.len() as u16 + 2;
            let (_, col) = manager.fields[field].viewport(rect.width.saturating_sub(prefix + 2));
            cursor = Some(crate::protocol::CursorState {
                x: (rect.x + prefix + col).min(rect.right() - 1),
                y: rect.y,
                visible: true,
                shape: 6,
            });
        }
    }
    if matches!(manager.page, Page::Running)
        || matches!(manager.page, Page::Done) && !manager.success
    {
        if let Some(screen) = &manager.screen {
            let view = Rect::new(
                inner.x,
                inner.y + 2,
                inner.width,
                inner.height.saturating_sub(6),
            );
            for yy in 0..view.height.min(screen.area.height) {
                for xx in 0..view.width.min(screen.area.width) {
                    buffer[(view.x + xx, view.y + yy)] = screen[(xx, yy)].clone();
                }
            }
            cursor = manager
                .cursor
                .filter(|(x, y)| *x < view.width && *y < view.height)
                .map(|(x, y)| crate::protocol::CursorState {
                    x: view.x + x,
                    y: view.y + y,
                    visible: true,
                    shape: 2,
                });
        } else {
            put_text(buffer,inner.x,y,inner.width,"Connecting… SSH authentication, installation approval and session selection stay here.",normal);
        }
    }
    if let Some(error) = &manager.error {
        put_text(
            buffer,
            inner.x,
            inner.bottom().saturating_sub(3),
            inner.width,
            error,
            accent,
        );
    }
    if matches!(manager.page, Page::Done) && !manager.success {
        let rect = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1);
        put_text(buffer, rect.x, rect.y, rect.width, "[ Retry ]", accent);
        hits.push((rect, Hit::Retry));
    }
    let primary = match manager.page {
        Page::Done if manager.import_workspace.is_some() => Some(("[ Open ]", Hit::OpenImported)),
        Page::Done if matches!(manager.last_operation, Some(Operation::EnableImports(_))) => {
            Some(("[ Sessions ]", Hit::Imports))
        }
        Page::Add | Page::Join => Some(("[ Connect ]", Hit::Connect)),
        Page::Rename(_) => Some(("[ Save ]", Hit::Connect)),
        Page::Confirm { .. } | Page::DirectConfirm { .. } => Some(("[ Confirm ]", Hit::Confirm)),
        _ => None,
    };
    if let Some((label, hit)) = primary {
        let rect = Rect::new(inner.x, inner.bottom() - 2, inner.width.min(16), 1);
        put_text(buffer, rect.x, rect.y, rect.width, label, accent);
        hits.push((rect, hit));
    }
    if matches!(manager.page, Page::Add | Page::Join) {
        let width = inner.width.saturating_sub(18);
        if width >= 12 {
            let rect = Rect::new(inner.x + 18, inner.bottom() - 2, width, 1);
            put_text(
                buffer,
                rect.x,
                rect.y,
                rect.width,
                if width >= 28 {
                    "[ Browse Tailscale (Ctrl+T) ]"
                } else {
                    "[ Tailscale ]"
                },
                accent,
            );
            hits.push((rect, Hit::BrowseTailscale));
        }
    }
    let back = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(1),
        16.min(inner.width),
        1,
    );
    put_text(
        buffer,
        back.x,
        back.y,
        back.width,
        if matches!(manager.page, Page::Running | Page::Approval(_)) {
            "[ Cancel ]"
        } else if matches!(
            manager.page,
            Page::Tailscale { .. } | Page::DirectConfirm { .. }
        ) {
            "[ Back ]"
        } else {
            "[ Servers ]"
        },
        normal,
    );
    hits.push((back, Hit::Back));
    let close = Rect::new(
        inner.right().saturating_sub(10),
        inner.bottom().saturating_sub(1),
        10.min(inner.width),
        1,
    );
    put_text(buffer, close.x, close.y, close.width, "[ Close ]", normal);
    hits.push((close, Hit::Close));
    Some(super::render::overlays::OverlayRender {
        area,
        machine_actions: hits,
        cursor,
        ..Default::default()
    })
}

pub(super) fn paint_launcher(
    buffer: &mut Buffer,
    sidebar: Rect,
    palette: &Palette,
    hits: &mut ShellHitMap,
) {
    if sidebar.width < 3 || sidebar.height == 0 {
        return;
    }
    // Use the gap between the existing New and Menu controls. Their hit boxes
    // and attention badges must remain intact.
    let rect = if hits.new_workspace.height > 0 && hits.global_launcher.height > 0 {
        if hits
            .global_launcher
            .x
            .saturating_sub(hits.new_workspace.right())
            < 3
        {
            // Long endpoint labels may consume the gap; retain the New action
            // and use its short label so the server control remains available.
            hits.new_workspace.width = hits.new_workspace.width.min(5);
            let start = hits.new_workspace.right();
            if start < hits.global_launcher.x {
                buffer.set_style(
                    Rect::new(
                        start,
                        hits.new_workspace.y,
                        hits.global_launcher.x - start,
                        1,
                    ),
                    Style::default().bg(palette.sidebar_bg),
                );
                for x in start..hits.global_launcher.x {
                    buffer[(x, hits.new_workspace.y)].set_symbol(" ");
                }
            }
        }
        let left = hits.new_workspace.right();
        let available = hits.global_launcher.x.saturating_sub(left + 1);
        let width = if available >= 8 {
            8
        } else if available >= 2 {
            2
        } else {
            return;
        };
        Rect::new(
            hits.global_launcher.x - width - 1,
            hits.new_workspace.y,
            width,
            1,
        )
    } else {
        // The compact rail shares its bottom row with the expand control.
        let y = sidebar.bottom() - 1;
        if hits.sidebar_toggle.height > 0 {
            let old = hits.sidebar_toggle;
            let cell = buffer[(old.x, old.y)].clone();
            buffer[(old.x, old.y)].reset();
            buffer[(old.x, old.y)].set_bg(palette.sidebar_bg);
            hits.sidebar_toggle.x = sidebar.right().saturating_sub(2);
            buffer[(hits.sidebar_toggle.x, hits.sidebar_toggle.y)] = cell;
        }
        Rect::new(sidebar.x, y, 2, 1)
    };
    super::render::put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width,
        if rect.width == 8 { "＋ server" } else { "+S" },
        Style::default().fg(palette.accent).bg(palette.sidebar_bg),
    );
    hits.add_machine = rect;
}

fn terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    (
        96.min(cols.saturating_sub(4)).saturating_sub(2).max(1),
        32.min(rows.saturating_sub(2)).saturating_sub(8).max(1),
    )
}
impl ClientShellState {
    pub(crate) fn machine_resize(&self, cols: u16, rows: u16) -> Option<Command> {
        let Some(ClientShellOverlay::Machines(manager)) = &self.overlay else {
            return None;
        };
        if !matches!(manager.page, Page::Running) {
            return None;
        }
        let (cols, rows) = terminal_size(cols, rows);
        Some(Command::Resize {
            id: manager.id,
            cols,
            rows,
        })
    }
}

fn confirmation_fits(text: &str, size: (u16, u16)) -> bool {
    use ratatui::widgets::{Paragraph, Wrap};
    let Some(area) = super::render::overlays::popup(Rect::new(0, 0, size.0, size.1), 96, 32) else {
        return false;
    };
    let width = area.width.saturating_sub(2);
    let height = area.height.saturating_sub(8);
    width >= 20
        && Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .line_count(width)
            <= usize::from(height)
}
fn small_window(buffer: &mut Buffer, palette: &Palette) -> super::render::overlays::OverlayRender {
    let rect = Rect::new(
        buffer.area.x,
        buffer.area.y,
        buffer.area.width,
        buffer.area.height.min(1),
    );
    if rect.height > 0 {
        super::render::put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            "Resize window · click to cancel",
            Style::default().fg(palette.accent),
        );
    }
    super::render::overlays::OverlayRender {
        area: buffer.area,
        machine_actions: vec![(rect, Hit::Close)],
        ..Default::default()
    }
}

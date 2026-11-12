use crate::api::schema::{
    ExternalTerminalSession, TerminalSessionIdentity, TerminalSessionsReport,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{self, Read},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

const HELPER: &str = "/usr/local/libexec/herdr-terminal-import";
const MAX_PROCESSES: usize = 32768;

#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    ppid: u32,
    pgrp: u32,
    sid: u32,
    tty: i64,
    foreground: i64,
    start: u64,
    command: String,
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions.
    unsafe { libc::getuid() }
}
fn limited(path: impl AsRef<Path>, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("Process metadata exceeds size limit"));
    }
    Ok(bytes)
}
fn parse_stat(text: &str) -> Option<Process> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text[..open].trim().parse().ok()?;
    let fields: Vec<_> = text[close + 1..].split_whitespace().collect();
    if fields.len() < 20 || matches!(fields[0], "Z" | "X") {
        return None;
    }
    Some(Process {
        pid,
        ppid: fields[1].parse().ok()?,
        pgrp: fields[2].parse().ok()?,
        sid: fields[3].parse().ok()?,
        tty: fields[4].parse().ok()?,
        foreground: fields[5].parse().ok()?,
        start: fields[19].parse().ok()?,
        command: text[open + 1..close]
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect(),
    })
}
fn process(pid: u32, owner: u32) -> io::Result<Process> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    if fs::metadata(&root)?.uid() != owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Terminal belongs to another account",
        ));
    }
    // PTY numbers can collide across devpts mounts. Never let reptyr choose
    // a master from a different container or process namespace.
    for namespace in ["mnt", "pid"] {
        if fs::read_link(root.join("ns").join(namespace))?
            != fs::read_link(Path::new("/proc/self/ns").join(namespace))?
        {
            return Err(io::Error::other(
                "Terminal belongs to a different process or mount namespace",
            ));
        }
    }
    let bytes = limited(root.join("stat"), 16384)?;
    parse_stat(&String::from_utf8_lossy(&bytes))
        .ok_or_else(|| io::Error::other("Terminal process is no longer running"))
}
fn boot() -> io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}
fn identity(p: &Process, owner: u32, boot: &str) -> TerminalSessionIdentity {
    TerminalSessionIdentity {
        boot_id: boot.into(),
        uid: owner,
        pid: p.pid,
        start_ticks: p.start,
        session_id: p.sid,
        tty: p.tty,
    }
}
fn validate_for(source: &TerminalSessionIdentity, owner: u32) -> io::Result<()> {
    if source.uid != owner || source.boot_id != boot()? {
        return Err(io::Error::other(
            "Terminal account or host boot changed; refresh the list",
        ));
    }
    let current = process(source.pid, owner)?;
    let leader = process(current.sid, owner)?;
    if leader.sid != current.sid {
        return Err(io::Error::other(
            "Terminal session leader changed; refresh the list",
        ));
    }
    if identity(&current, owner, &source.boot_id) != *source || current.tty == 0 {
        return Err(io::Error::other(
            "Terminal process changed or exited; refresh the list",
        ));
    }
    Ok(())
}
fn protected(path: &Path) -> bool {
    path.ancestors().all(|part| {
        fs::symlink_metadata(part).is_ok_and(|metadata| {
            !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && metadata.permissions().mode() & 0o022 == 0
        })
    }) && path.is_file()
        && fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}
fn reptyr() -> Option<PathBuf> {
    ["/usr/bin/reptyr", "/usr/local/bin/reptyr"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| protected(path))
}

fn master_tty(text: &str) -> Option<i64> {
    let index: u32 = text
        .lines()
        .find_map(|line| line.strip_prefix("tty-index:"))?
        .trim()
        .parse()
        .ok()?;
    if index >= 1 << 20 {
        return None;
    }
    // Unix98 slaves use major 136, minor index. proc stat exposes the
    // kernel's signed 32-bit device encoding, including high minor bits.
    Some(libc::makedev(136, index) as u32 as i32 as i64)
}

fn held_master_ttys(pid: u32) -> io::Result<HashSet<i64>> {
    let entries = match fs::read_dir(format!("/proc/{pid}/fd")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => return Err(error),
    };
    let mut devices = HashSet::new();
    for (index, entry) in entries.enumerate() {
        if index >= 32768 {
            return Err(io::Error::other(
                "Too many open descriptors to safely exclude Herdr terminals",
            ));
        }
        let entry = entry?;
        let result = (|| {
            let metadata = fs::metadata(entry.path())?;
            // Skip regular files and large epoll fdinfo. Unix98 PTY masters
            // are ptmx character devices (5:2), regardless of their path.
            if !metadata.file_type().is_char_device()
                || libc::major(metadata.rdev()) != 5
                || libc::minor(metadata.rdev()) != 2
            {
                return Ok(None);
            }
            let bytes = limited(
                PathBuf::from(format!("/proc/{pid}/fdinfo")).join(entry.file_name()),
                4096,
            )?;
            master_tty(&String::from_utf8_lossy(&bytes))
                .map(Some)
                .ok_or_else(|| {
                    io::Error::other(
                        "Cannot identify an owned PTY master; terminal discovery stopped",
                    )
                })
        })();
        match result {
            Ok(Some(device)) => {
                devices.insert(device);
            }
            Ok(None) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(devices)
}

pub(crate) fn discover() -> io::Result<TerminalSessionsReport> {
    let owner = uid();
    let host_boot = boot()?;
    let entries: Vec<_> = fs::read_dir("/proc")?.take(MAX_PROCESSES + 1).collect();
    if entries.len() > MAX_PROCESSES {
        return Err(io::Error::other(
            "Too many processes to safely discover external terminals",
        ));
    }
    let processes: HashMap<u32, Process> = entries
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| process(pid, owner).ok().map(|p| (pid, p)))
        .collect();
    // Live handoff reparents existing pane shells to init, so ancestry alone
    // cannot identify them. The daemon still owns their PTY masters.
    let mut managed_ttys = held_master_ttys(std::process::id())?;
    for p in processes.values().filter(|p| {
        p.pid != std::process::id() && matches!(p.command.as_str(), "herdr" | "herdr-dev")
    }) {
        managed_ttys.extend(held_master_ttys(p.pid)?);
    }
    let mut excluded = HashSet::new();
    for p in processes.values() {
        if let Ok(bytes) = limited(format!("/proc/{}/cmdline", p.pid), 8192) {
            let fields: Vec<_> = bytes.split(|b| *b == 0).collect();
            if fields.get(1) == Some(&b"terminal-import".as_slice())
                && fields.get(2) == Some(&b"run".as_slice())
            {
                if let Some(source) = fields
                    .get(3)
                    .and_then(|token| std::str::from_utf8(token).ok())
                    .and_then(|token| crate::terminal_import::decode(token).ok())
                {
                    if source.uid == owner
                        && source.boot_id == host_boot
                        && validate_for(&source, owner).is_ok()
                    {
                        excluded.insert((source.tty, source.session_id));
                    }
                }
            }
        }
    }
    let mut groups: BTreeMap<(i64, u32), Vec<&Process>> = BTreeMap::new();
    for p in processes.values().filter(|p| p.tty != 0 && p.sid != 0) {
        groups.entry((p.tty, p.sid)).or_default().push(p);
    }
    let mut sessions = Vec::new();
    for (key, mut members) in groups {
        if excluded.contains(&key) || managed_ttys.contains(&key.0) {
            continue;
        }
        // A same-user job launched with sudo inside another account's terminal
        // does not own that whole terminal. Require its live session leader.
        if processes
            .get(&key.1)
            .is_none_or(|leader| leader.sid != key.1)
        {
            continue;
        }
        let managed = members.iter().any(|process| {
            let mut cursor = Some(process.pid);
            let mut visited = HashSet::new();
            while let Some(pid) = cursor {
                if !visited.insert(pid) {
                    break;
                }
                let Some(parent) = processes.get(&pid) else {
                    break;
                };
                if matches!(
                    parent.command.as_str(),
                    "herdr" | "herdr-dev" | "reptyr" | "tmux" | "screen"
                ) {
                    return true;
                }
                cursor = Some(parent.ppid);
            }
            false
        });
        if managed {
            continue;
        }
        members.sort_by_key(|p| p.pid);
        let leader = members
            .iter()
            .find(|p| p.pid == key.1)
            .copied()
            .unwrap_or(members[0]);
        let foreground = members
            .iter()
            .rev()
            .find(|p| p.pgrp as i64 == leader.foreground)
            .copied()
            .unwrap_or(leader);
        let terminal = (0..3)
            .filter_map(|fd| fs::read_link(format!("/proc/{}/fd/{fd}", leader.pid)).ok())
            .find(|path| path.starts_with("/dev/pts") || path.starts_with("/dev/tty"))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("tty {}", key.0));
        let cwd = fs::read_link(format!("/proc/{}/cwd", foreground.pid))
            .map(|path| {
                path.to_string_lossy()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(1024)
                    .collect()
            })
            .unwrap_or_default();
        sessions.push(ExternalTerminalSession {
            identity: identity(leader, owner, &host_boot),
            terminal,
            command: foreground.command.clone(),
            cwd,
            process_count: members.len(),
        });
        if sessions.len() == 256 {
            break;
        }
    }
    let ready = reptyr().is_some() && protected(Path::new(HELPER));
    Ok(TerminalSessionsReport{supported:true,helper_ready:ready,message:if ready{"Import moves the whole terminal without restarting its programs. Administrator authorization may be requested in the new workspace. Existing scrollback is not copied."}else{"Enable the import helper on this host first. This installs system reptyr and a root-owned Herdr helper; administrator access is required."}.into(),sessions})
}

pub(crate) fn validate(source: &TerminalSessionIdentity) -> io::Result<()> {
    validate_for(source, uid())?;
    if !discover()?
        .sessions
        .iter()
        .any(|session| session.identity == *source)
    {
        return Err(io::Error::other(
            format!("Terminal is already managed, being imported, or no longer available. Check the Imported terminal {} workspace before retrying.",source.pid),
        ));
    }
    if reptyr().is_none() || !protected(Path::new(HELPER)) {
        return Err(io::Error::other("Enable this server's import helper first"));
    }
    Ok(())
}

fn administrator(program: &str, args: &[&std::ffi::OsStr]) -> io::Result<()> {
    let mut command = if uid() == 0 {
        Command::new(program)
    } else {
        let mut c = Command::new("/usr/bin/sudo");
        c.arg("--").arg(program);
        c
    };
    if !command.args(args).status()?.success() {
        return Err(io::Error::other("Administrator setup did not finish"));
    }
    Ok(())
}
pub(crate) fn install_helper() -> io::Result<()> {
    use std::ffi::OsStr;
    if reptyr().is_none() {
        let options = [
            ("/usr/bin/apt-get", vec!["install", "-y", "reptyr"]),
            ("/usr/bin/dnf", vec!["install", "-y", "reptyr"]),
            (
                "/usr/bin/pacman",
                vec!["-S", "--needed", "--noconfirm", "reptyr"],
            ),
            ("/sbin/apk", vec!["add", "reptyr"]),
        ];
        let Some((manager, args)) = options
            .into_iter()
            .find(|(program, _)| Path::new(program).is_file())
        else {
            return Err(io::Error::other("Install the reptyr package using this server's package manager, then enable the helper again"));
        };
        administrator(manager, &args.iter().map(OsStr::new).collect::<Vec<_>>())?;
    }
    if reptyr().is_none() {
        return Err(io::Error::other(
            "A root-owned system reptyr executable is required",
        ));
    }
    // Do not install through a preexisting symlink or a user-writable parent.
    for parent in Path::new(HELPER)
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
    {
        let m = match fs::symlink_metadata(parent) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if m.file_type().is_symlink() || m.uid() != 0 || m.permissions().mode() & 0o022 != 0 {
            return Err(io::Error::other(
                "Import helper directory must be root-owned and not writable by other users",
            ));
        }
    }
    if !Path::new("/usr/local/libexec").exists() {
        administrator(
            "/usr/bin/install",
            &[
                OsStr::new("-d"),
                OsStr::new("-m"),
                OsStr::new("0755"),
                OsStr::new("/usr/local/libexec"),
            ],
        )?;
    }
    if fs::symlink_metadata(HELPER).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other("Refusing a symlinked import helper"));
    }
    administrator(
        "/usr/bin/install",
        &[
            OsStr::new("-m"),
            OsStr::new("0755"),
            std::env::current_exe()?.as_os_str(),
            OsStr::new(HELPER),
        ],
    )?;
    if !protected(Path::new(HELPER)) {
        return Err(io::Error::other("Could not secure the import helper"));
    }
    println!("Terminal import helper is ready. Kernel tracing settings were not changed.");
    Ok(())
}

pub(crate) fn run(source: &TerminalSessionIdentity, privileged: bool) -> io::Result<()> {
    let token = crate::terminal_import::encode(source)?;
    if privileged {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0
            || !protected(Path::new(HELPER))
            || fs::canonicalize(std::env::current_exe()?)? != fs::canonicalize(HELPER)?
        {
            return Err(io::Error::other(
                "Use the installed root-owned terminal import helper",
            ));
        }
        let caller = std::env::var("SUDO_UID")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        validate_for(source, caller)?;
        let tool = reptyr().ok_or_else(|| io::Error::other("System reptyr is unavailable"))?;
        let status = Command::new(tool)
            .args(["-T", &source.pid.to_string()])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("Terminal transfer failed; inspect the reptyr diagnostic above. No program restart was requested."));
        }
        return Ok(());
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(io::Error::other("Terminal import requires a Herdr pane"));
    }
    loop {
        validate_for(source, uid())?;
        println!(
            "Importing the existing terminal for PID {}. Programs keep their original process IDs.",
            source.pid
        );
        let mut command = if uid() == 0 {
            Command::new(HELPER)
        } else {
            let mut c = Command::new("/usr/bin/sudo");
            c.arg("--").arg(HELPER);
            c
        };
        let result = command
            .args(["terminal-import", "privileged-run", &token])
            .status();
        if result.as_ref().is_ok_and(|status| status.success()) {
            return Ok(());
        }
        println!("\r\nImport did not finish. Keep this workspace open to inspect the error. Press Enter to retry, or close this workspace to stop trying.");
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input)? == 0 {
            return Err(io::Error::other("Import input closed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn master_devices_cover_high_minor_numbers_and_reject_invalid_metadata() {
        assert_eq!(master_tty("pos:\t0\ntty-index:\t6\n"), Some(34822));
        assert_eq!(master_tty("tty-index: 256\n"), Some(0x108800));
        assert_eq!(master_tty("tty-index: 1048575\n"), Some(-1_013_505));
        assert_eq!(master_tty("tty-index: 1048576\n"), None);
        assert_eq!(master_tty("tty-index: -1\n"), None);
        assert_eq!(master_tty("pos: 6\n"), None);
    }
    #[test]
    fn held_master_exclusion_does_not_depend_on_shell_ancestry() {
        use std::os::fd::{FromRawFd, OwnedFd};
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: output pointers are valid and optional arguments are null.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        // SAFETY: openpty returned two fresh owned descriptors.
        let (_master, _slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let tty = fs::metadata(format!("/proc/self/fd/{slave}"))
            .unwrap()
            .rdev() as u32 as i32 as i64;
        assert!(held_master_ttys(std::process::id()).unwrap().contains(&tty));
    }
    #[test]
    fn stat_parser_handles_parentheses_spaces_stale_and_zombie_processes() {
        let mut fields = vec!["0"; 22];
        fields[0] = "S";
        fields[1] = "4";
        fields[2] = "7";
        fields[3] = "7";
        fields[4] = "34817";
        fields[5] = "8";
        fields[19] = "999";
        let p = parse_stat(&format!("7 (shell (long) name) {}", fields.join(" "))).unwrap();
        assert_eq!(
            (p.pid, p.ppid, p.sid, p.tty, p.start),
            (7, 4, 7, 34817, 999)
        );
        assert_eq!(p.command, "shell (long) name");
        fields[0] = "Z";
        assert!(parse_stat(&format!("7 (z) {}", fields.join(" "))).is_none());
    }
    #[test]
    fn stale_identity_is_rejected_without_tracing_or_signaling() {
        let current = process(std::process::id(), uid()).unwrap();
        let mut source = identity(&current, uid(), &boot().unwrap());
        source.start_ticks += 1;
        assert!(validate_for(&source, uid()).is_err());
        source.uid = uid() + 1;
        assert!(validate_for(&source, uid()).is_err());
    }
}

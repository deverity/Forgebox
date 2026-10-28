//! What each allowlisted operation does (docs/11 §1, ADR-0024). Every function first reads
//! the current state and builds the plan (also the `--dry-run` answer), then, unless it is
//! a dry run, changes the system through [`PrivilegedOps`] and reports what it did.
//!
//! The operation reaching this module was validated twice by the caller: lexically against
//! the request's root, and again after every path was canonicalized (junctions resolved).

use std::io::Read as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::error::{ForgeError, IoResultExt, Result};
use forge_core::helper::hosts::{apply_hosts_block_bytes, read_hosts_block};
use forge_core::helper::trust::{check_machine_root_candidate, check_machine_root_removal};
use forge_core::helper::{HelperOp, HostsEntry};
use forge_core::ids::RequestId;
use forge_core::paths::plain_path;
use forge_core::platform::{
    CaseSensitiveFs, CaseSensitivitySupport, CertThumbprint, CertificateInfo, CodeSign,
    CodeSignInfo, FORGEBOX_NRPT_COMMENT, FirewallRuleSpec, NoFollow, NrptRule, NrptSource,
    PrivilegedOps, VCREDIST_INSTALL_ARGS, VCREDIST_PUBLISHER, open_no_follow,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Firewall group of the helper's rules (Windows Firewall console).
pub const FIREWALL_GROUP: &str = "Forgebox";
/// Longest the VC++ installer may run.
pub const INSTALLER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Biggest `hosts` file the helper rewrites.
const MAX_HOSTS_BYTES: usize = 16 * 1024 * 1024;
/// Biggest certificate file read from `certs/ca/`.
const MAX_CERT_BYTES: u64 = 64 * 1024;
/// Extensions of certificate files in `certs/ca/`.
const CERT_EXTENSIONS: [&str; 3] = ["crt", "pem", "cer"];

/// Everything an operation may use.
pub struct Env<'a> {
    /// Canonical root, without `\\?\`.
    pub root: &'a Path,
    pub sys: &'a dyn PrivilegedOps,
    pub case_fs: &'a dyn CaseSensitiveFs,
    pub code_sign: &'a dyn CodeSign,
    pub request_id: RequestId,
    pub dry_run: bool,
}

/// Result of an operation: message (English, for logs) and details for the daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub message: String,
    pub details: Value,
}

impl Outcome {
    fn new(message: impl Into<String>, details: Value) -> Self {
        Self {
            message: message.into(),
            details,
        }
    }
}

/// Runs (or, in a dry run, plans) `op`.
pub fn run(env: &Env<'_>, op: &HelperOp) -> Result<Outcome> {
    match op {
        HelperOp::NrptAdd {
            namespace,
            nameserver,
        } => nrpt_add(env, namespace, *nameserver),
        HelperOp::NrptRemove { namespace } => nrpt_remove(env, namespace),
        HelperOp::HostsSetBlock { entries } => hosts_set_block(env, entries),
        HelperOp::TrustAddMachineRoot { thumbprint } => trust_add(env, &thumbprint.parse()?),
        HelperOp::TrustRemoveMachineRoot { thumbprint } => trust_remove(env, &thumbprint.parse()?),
        HelperOp::FirewallAdd { name, port, exe } => firewall_add(env, name, *port, exe),
        HelperOp::FirewallRemove { name } => firewall_remove(env, name),
        HelperOp::DefenderExclude { path } => defender(env, path, true),
        HelperOp::DefenderRemove { path } => defender(env, path, false),
        HelperOp::VcredistInstall { path, sha256 } => vcredist_install(env, path, sha256),
        HelperOp::FsSetCaseSensitive { path } => set_case_sensitive(env, path),
    }
}

fn verb(env: &Env<'_>, done: &str, planned: &str) -> String {
    if env.dry_run {
        format!("dry run: would {planned}")
    } else {
        done.to_owned()
    }
}

/// Flushes the resolver cache after a DNS change; a failure is reported, not fatal (the
/// change itself is done and the cache expires on its own).
fn flush(env: &Env<'_>, details: &mut Value) {
    if let Err(err) = env.sys.flush_dns_cache() {
        details["flush_error"] = Value::String(err.to_string());
    }
}

/// The Forgebox rules for exactly `namespace` (the only ones the helper replaces/removes).
fn own_rules<'r>(rules: &'r [NrptRule], namespace: &str) -> Vec<&'r NrptRule> {
    rules
        .iter()
        .filter(|r| r.is_forgebox() && r.namespaces.len() == 1 && r.namespaces[0] == namespace)
        .collect()
}

fn nrpt_add(env: &Env<'_>, namespace: &str, nameserver: IpAddr) -> Result<Outcome> {
    let table = env.sys.nrpt_table()?;
    let rules = &table.rules;
    let ours = own_rules(rules, namespace);
    let up_to_date = ours.len() == 1 && ours[0].nameservers == [nameserver];
    let replace: Vec<&str> = if up_to_date {
        Vec::new()
    } else {
        ours.iter().map(|r| r.id.as_str()).collect()
    };
    let foreign: Vec<&str> = rules
        .iter()
        .filter(|r| {
            r.source == NrptSource::Local
                && !r.is_forgebox()
                && r.namespaces.iter().any(|n| n == namespace)
        })
        .map(|r| r.id.as_str())
        .collect();
    let ignored = table.local_rules_ignored();
    let mut details = json!({
        "namespace": namespace,
        "nameserver": nameserver.to_string(),
        "comment": FORGEBOX_NRPT_COMMENT,
        "already_present": up_to_date,
        "replace_rules": replace,
        "foreign_rules": foreign,
        "group_policy_rules": table.group_policy_rules(),
        "group_policy_table": table.group_policy_table,
        // Windows ignores local rules while the Group Policy table exists, even empty:
        // the daemon falls back to the hosts block (docs/06 §5.4, ADR-0033).
        "effective": ignored.is_none(),
        "reason": ignored.map(|r| r.as_str()),
    });
    if env.dry_run {
        let plan = if up_to_date {
            format!("keep the NRPT rule {namespace} -> {nameserver} (already present)")
        } else {
            format!("add the NRPT rule {namespace} -> {nameserver}")
        };
        return Ok(Outcome::new(format!("dry run: would {plan}"), details));
    }
    if up_to_date {
        details["rule_id"] = json!(ours[0].id);
        flush(env, &mut details);
        return Ok(Outcome::new(
            format!("NRPT rule {namespace} -> {nameserver} already present"),
            details,
        ));
    }
    for id in &replace {
        env.sys.nrpt_remove(id)?;
    }
    env.sys
        .nrpt_add(namespace, nameserver, FORGEBOX_NRPT_COMMENT)?;
    let after = env.sys.nrpt_table()?;
    let created = own_rules(&after.rules, namespace)
        .into_iter()
        .find(|r| r.nameservers.contains(&nameserver))
        .map(|r| r.id.clone());
    details["rule_id"] = json!(created);
    flush(env, &mut details);
    Ok(Outcome::new(
        format!("NRPT rule {namespace} -> {nameserver} added"),
        details,
    ))
}

fn nrpt_remove(env: &Env<'_>, namespace: &str) -> Result<Outcome> {
    let table = env.sys.nrpt_table()?;
    let ids: Vec<String> = own_rules(&table.rules, namespace)
        .into_iter()
        .map(|r| r.id.clone())
        .collect();
    let mut details = json!({ "namespace": namespace, "remove_rules": ids });
    if !env.dry_run {
        for id in &ids {
            env.sys.nrpt_remove(id)?;
        }
        flush(env, &mut details);
    }
    let message = if ids.is_empty() {
        format!("no Forgebox NRPT rule for {namespace}")
    } else {
        verb(
            env,
            &format!("NRPT rule for {namespace} removed"),
            &format!("remove {} NRPT rule(s) for {namespace}", ids.len()),
        )
    };
    Ok(Outcome::new(message, details))
}

fn hosts_set_block(env: &Env<'_>, entries: &[HostsEntry]) -> Result<Outcome> {
    let path = env.sys.hosts_path();
    let current = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(ForgeError::io_at("reading", &path, e)),
    };
    if current.len() > MAX_HOSTS_BYTES {
        return Err(ForgeError::ConfigInvalid(format!(
            "{} has {} bytes; the helper rewrites files up to {MAX_HOSTS_BYTES}",
            path.display(),
            current.len()
        )));
    }
    let had_block = read_hosts_block(&String::from_utf8_lossy(&current))?.is_some();
    let updated = apply_hosts_block_bytes(&current, entries)?;
    let action = if updated == current {
        "unchanged"
    } else if entries.is_empty() {
        "remove"
    } else if had_block {
        "replace"
    } else {
        "add"
    };
    let mut details = json!({
        "path": path.display().to_string(),
        "entries": entries.len(),
        "action": action,
    });
    if env.dry_run {
        return Ok(Outcome::new(
            format!(
                "dry run: would {action} the hosts block ({} entries)",
                entries.len()
            ),
            details,
        ));
    }
    if action != "unchanged" {
        env.sys.replace_system_file(&path, &updated)?;
    }
    flush(env, &mut details);
    Ok(Outcome::new(
        format!("hosts block: {action} ({} entries)", entries.len()),
        details,
    ))
}

/// The certificate of `certs/ca/` with this thumbprint.
fn find_ca_certificate(
    env: &Env<'_>,
    thumbprint: &CertThumbprint,
) -> Result<(PathBuf, CertificateInfo)> {
    let dir = env.root.join("certs").join("ca");
    let entries = std::fs::read_dir(&dir).at("listing", &dir)?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| CERT_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .collect();
    candidates.sort();
    for path in candidates {
        let Ok(opened) = open_no_follow(&path, NoFollow::Read) else {
            continue;
        };
        let mut bytes = Vec::new();
        if opened
            .file
            .take(MAX_CERT_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        if let Ok(info) = env.sys.certificate_info(&bytes) {
            if info.thumbprint == *thumbprint {
                return Ok((path, info));
            }
        }
    }
    Err(ForgeError::NotFound(format!(
        "no certificate with thumbprint {thumbprint} in {}",
        dir.display()
    )))
}

fn trust_add(env: &Env<'_>, thumbprint: &CertThumbprint) -> Result<Outcome> {
    let (path, info) = find_ca_certificate(env, thumbprint)?;
    check_machine_root_candidate(&info, thumbprint)?;
    let already = env.sys.machine_root_find(thumbprint)?.is_some();
    let details = json!({
        "thumbprint": info.thumbprint,
        "certificate": path.display().to_string(),
        "subject": info.subject,
        "name_constraints": info.name_constraints,
        "already_trusted": already,
    });
    if env.dry_run {
        return Ok(Outcome::new(
            format!(
                r"dry run: would trust {:?} in LocalMachine\Root",
                info.subject
            ),
            details,
        ));
    }
    if !already {
        env.sys.machine_root_add(&info.der)?;
        if env.sys.machine_root_find(thumbprint)?.is_none() {
            return Err(ForgeError::Internal(format!(
                r"{thumbprint} is not in LocalMachine\Root after adding it"
            )));
        }
    }
    Ok(Outcome::new(
        format!(r"{:?} trusted in LocalMachine\Root", info.subject),
        details,
    ))
}

fn trust_remove(env: &Env<'_>, thumbprint: &CertThumbprint) -> Result<Outcome> {
    let Some(info) = env.sys.machine_root_find(thumbprint)? else {
        return Ok(Outcome::new(
            format!(r"{thumbprint} is not in LocalMachine\Root"),
            json!({ "thumbprint": thumbprint, "present": false }),
        ));
    };
    check_machine_root_removal(&info)?;
    let details = json!({
        "thumbprint": info.thumbprint,
        "subject": info.subject,
        "present": true,
    });
    if !env.dry_run {
        env.sys.machine_root_remove(thumbprint)?;
    }
    Ok(Outcome::new(
        verb(
            env,
            &format!(r"{:?} removed from LocalMachine\Root", info.subject),
            &format!(r"remove {:?} from LocalMachine\Root", info.subject),
        ),
        details,
    ))
}

fn firewall_add(env: &Env<'_>, name: &str, port: u16, exe: &Path) -> Result<Outcome> {
    let meta = std::fs::metadata(exe).at("inspecting", exe)?;
    if !meta.is_file() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a file",
            exe.display()
        )));
    }
    let exe = plain_path(exe);
    let program = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rule = FirewallRuleSpec {
        name: name.to_owned(),
        description: format!("Forgebox: inbound TCP {port} for {program} (LAN access)"),
        grouping: FIREWALL_GROUP.to_owned(),
        exe,
        port,
    };
    let existing = env.sys.firewall_rule_count(name)?;
    let mut details = json!({
        "rule": rule,
        "profiles": ["private", "domain"],
        "replaces": existing,
    });
    if env.dry_run {
        return Ok(Outcome::new(
            format!("dry run: would allow inbound TCP {port} for {program} ({name})"),
            details,
        ));
    }
    details["replaced"] = json!(env.sys.firewall_add(&rule)?);
    Ok(Outcome::new(
        format!("firewall rule {name} added (TCP {port}, {program})"),
        details,
    ))
}

fn firewall_remove(env: &Env<'_>, name: &str) -> Result<Outcome> {
    let existing = env.sys.firewall_rule_count(name)?;
    let mut details = json!({ "name": name, "existing": existing });
    if env.dry_run {
        return Ok(Outcome::new(
            format!("dry run: would remove {existing} firewall rule(s) named {name}"),
            details,
        ));
    }
    let removed = env.sys.firewall_remove(name)?;
    details["removed"] = json!(removed);
    Ok(Outcome::new(
        format!("{removed} firewall rule(s) named {name} removed"),
        details,
    ))
}

fn defender(env: &Env<'_>, path: &Path, add: bool) -> Result<Outcome> {
    let path = plain_path(path);
    let details = json!({ "path": path.display().to_string() });
    let what = if add {
        "add the Defender exclusion"
    } else {
        "remove the Defender exclusion"
    };
    if env.dry_run {
        return Ok(Outcome::new(
            format!("dry run: would {what} for {}", path.display()),
            details,
        ));
    }
    if add {
        env.sys.defender_add_exclusion(&path)?;
    } else {
        env.sys.defender_remove_exclusion(&path)?;
    }
    Ok(Outcome::new(
        format!(
            "Defender exclusion {} for {}",
            if add { "added" } else { "removed" },
            path.display()
        ),
        details,
    ))
}

/// The installer must carry a valid Authenticode signature by Microsoft.
fn check_signature(path: &Path, info: &CodeSignInfo) -> Result<()> {
    let publisher = info.publisher.as_deref().unwrap_or("");
    if info.signed && info.trusted && publisher == VCREDIST_PUBLISHER {
        return Ok(());
    }
    Err(ForgeError::VerificationFailed(format!(
        "{}: Authenticode signature signed={} trusted={} publisher={publisher:?} ({}); \
         expected a valid signature by {VCREDIST_PUBLISHER:?}",
        path.display(),
        info.signed,
        info.trusted,
        info.status.as_deref().unwrap_or("ok")
    )))
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut file = open_no_follow(path, NoFollow::ReadExclusive)?.file;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).at("reading", path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn vcredist_install(env: &Env<'_>, path: &Path, sha256: &str) -> Result<Outcome> {
    let shown = plain_path(path);
    let mut details = json!({
        "path": shown.display().to_string(),
        "sha256": sha256.to_ascii_lowercase(),
        "publisher": VCREDIST_PUBLISHER,
        "args": VCREDIST_INSTALL_ARGS,
    });
    if env.dry_run {
        let actual = sha256_of(path)?;
        if !actual.eq_ignore_ascii_case(sha256) {
            return Err(ForgeError::VerificationFailed(format!(
                "{}: sha256 is {actual}, expected {}",
                shown.display(),
                sha256.to_ascii_lowercase()
            )));
        }
        check_signature(&shown, &env.code_sign.verify(path)?)?;
        return Ok(Outcome::new(
            "dry run: would install the VC++ Redistributable (hash and signature verified)",
            details,
        ));
    }
    // Copy first, verify the copy, run the copy: the original stays writable by the user.
    let staged = env
        .sys
        .stage_verified_copy(path, sha256, &env.request_id.to_string())?;
    check_signature(staged.path(), &env.code_sign.verify(staged.path())?)?;
    let code = env
        .sys
        .run_installer(staged.path(), VCREDIST_INSTALL_ARGS, INSTALLER_TIMEOUT)?;
    details["exit_code"] = json!(code);
    let message = match code {
        0 => "VC++ Redistributable installed",
        3010 | 1641 => "VC++ Redistributable installed; Windows must restart to finish",
        1638 => "VC++ Redistributable: the same or a newer version is already installed",
        _ => {
            return Err(ForgeError::io(
                "installing the VC++ Redistributable",
                std::io::Error::other(format!("vc_redist exited with code {code}")),
            ));
        }
    };
    details["reboot_required"] = json!(matches!(code, 3010 | 1641));
    Ok(Outcome::new(message, details))
}

fn set_case_sensitive(env: &Env<'_>, path: &Path) -> Result<Outcome> {
    let meta = std::fs::metadata(path).at("inspecting", path)?;
    if !meta.is_dir() {
        return Err(ForgeError::InvalidInput(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    let shown = plain_path(path);
    let support = env.case_fs.support(path);
    let current = env.case_fs.get(path).ok();
    let details = json!({
        "path": shown.display().to_string(),
        "supported": support == CaseSensitivitySupport::Supported,
        "case_sensitive": current,
    });
    if let CaseSensitivitySupport::Unsupported(reason) = support {
        return Err(ForgeError::UnsupportedPlatform(reason));
    }
    if current == Some(true) {
        return Ok(Outcome::new(
            format!("{} is already case-sensitive", shown.display()),
            details,
        ));
    }
    if env.dry_run {
        return Ok(Outcome::new(
            format!("dry run: would make {} case-sensitive", shown.display()),
            details,
        ));
    }
    env.case_fs.set(path, true)?;
    Ok(Outcome::new(
        format!("{} is now case-sensitive", shown.display()),
        details,
    ))
}

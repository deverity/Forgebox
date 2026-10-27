//! What the helper accepts to trust (or distrust) for every user of the machine
//! (`trust.add_machine_root` / `trust.remove_machine_root`, docs/11 §1 and §3, ADR-0023).
//!
//! `certs/ca/` is writable by the user, so "the certificate in `certs/ca/` with this
//! thumbprint" alone would let any process of the user install **any** root CA for every
//! account. The helper therefore only adds a certificate that is the Forgebox CA as
//! docs/11 §3 defines it: a self-signed CA whose subject starts with `Forgebox` and whose
//! Name Constraints only permit names under suffixes reserved for local use
//! ([`crate::dns_name::LOCAL_USE_SUFFIXES`]) and loopback or private addresses. Even a
//! stolen key of such a CA cannot sign `google.com`. Removal is limited to certificates
//! whose subject starts with `Forgebox` (a request can never strip a corporate or a
//! Microsoft root from the machine).

use std::net::IpAddr;

use crate::dns_name;
use crate::error::{ForgeError, Result};
use crate::platform::{CertificateInfo, IpPrefix};

/// Prefix of the subject (CN) of every Forgebox CA (`Forgebox Local CA <host> <year>`,
/// docs/06 §6.1).
pub const CA_SUBJECT_PREFIX: &str = "Forgebox";

fn forbidden(info: &CertificateInfo, why: impl std::fmt::Display) -> ForgeError {
    ForgeError::Forbidden(format!(
        "certificate {:?} ({}) {why}",
        info.subject, info.thumbprint
    ))
}

/// Whether `prefix` stays inside loopback or private address space.
fn private_range(prefix: &IpPrefix) -> bool {
    let Some(len) = prefix.len else {
        return false;
    };
    let inside = |net: IpAddr, net_len: u8| -> bool {
        if len < net_len {
            return false;
        }
        match (prefix.addr, net) {
            (IpAddr::V4(a), IpAddr::V4(n)) => {
                let mask = u32::MAX.checked_shl(32 - u32::from(net_len)).unwrap_or(0);
                u32::from(a) & mask == u32::from(n) & mask
            }
            (IpAddr::V6(a), IpAddr::V6(n)) => {
                let mask = u128::MAX.checked_shl(128 - u32::from(net_len)).unwrap_or(0);
                u128::from(a) & mask == u128::from(n) & mask
            }
            _ => false,
        }
    };
    let nets: [(&str, u8); 8] = [
        ("127.0.0.0", 8),
        ("10.0.0.0", 8),
        ("172.16.0.0", 12),
        ("192.168.0.0", 16),
        ("169.254.0.0", 16),
        ("::1", 128),
        ("fc00::", 7),
        ("fe80::", 10),
    ];
    nets.iter()
        .any(|(net, net_len)| inside(net.parse().expect("constant network"), *net_len))
}

/// Checks that `info` (read from `certs/ca/`) may be added to `LocalMachine\Root` for the
/// request carrying `thumbprint`.
pub fn check_machine_root_candidate(info: &CertificateInfo, thumbprint: &str) -> Result<()> {
    if !info.thumbprint.eq_ignore_ascii_case(thumbprint) {
        return Err(forbidden(
            info,
            format_args!("does not have the requested thumbprint {thumbprint}"),
        ));
    }
    if !info.subject.starts_with(CA_SUBJECT_PREFIX) {
        return Err(forbidden(
            info,
            format_args!("is not a Forgebox CA (subject must start with {CA_SUBJECT_PREFIX:?})"),
        ));
    }
    if !info.self_signed {
        return Err(forbidden(info, "is not a self-signed root"));
    }
    if !info.is_ca {
        return Err(forbidden(
            info,
            "is not a CA (Basic Constraints cA is false)",
        ));
    }
    let Some(constraints) = &info.name_constraints else {
        return Err(forbidden(
            info,
            "has no Name Constraints: a machine-wide root must not be able to sign any name",
        ));
    };
    if constraints.permitted_dns.is_empty() {
        return Err(forbidden(info, "permits no DNS name subtree"));
    }
    for name in &constraints.permitted_dns {
        let bare = name.trim_start_matches('.').to_ascii_lowercase();
        if dns_name::check_name(&bare).is_err() || !dns_name::is_local_use(&bare) {
            return Err(forbidden(
                info,
                format_args!(
                    "permits {name:?}, which is not under a suffix reserved for local use"
                ),
            ));
        }
    }
    if let Some(ip) = constraints.permitted_ip.iter().find(|p| !private_range(p)) {
        return Err(forbidden(
            info,
            format_args!("permits {ip}, which is not loopback or private"),
        ));
    }
    if constraints.permitted_other > 0 {
        return Err(forbidden(
            info,
            "permits name forms other than DNS names and IP ranges",
        ));
    }
    Ok(())
}

/// Checks that the certificate found in `LocalMachine\Root` may be removed.
pub fn check_machine_root_removal(info: &CertificateInfo) -> Result<()> {
    if info.subject.starts_with(CA_SUBJECT_PREFIX) {
        Ok(())
    } else {
        Err(forbidden(
            info,
            format_args!("is not a Forgebox CA; the helper only removes its own roots"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::NameConstraints;

    fn prefix(addr: &str, len: u8) -> IpPrefix {
        IpPrefix {
            addr: addr.parse().unwrap(),
            len: Some(len),
        }
    }

    fn forgebox_ca() -> CertificateInfo {
        CertificateInfo {
            der: vec![1, 2, 3],
            thumbprint: "ab".repeat(20),
            subject: "Forgebox Local CA devbox 2026".into(),
            self_signed: true,
            is_ca: true,
            name_constraints: Some(NameConstraints {
                critical: true,
                permitted_dns: vec!["test".into(), ".localhost".into()],
                permitted_ip: vec![prefix("127.0.0.0", 8), prefix("::1", 128)],
                permitted_other: 0,
                excluded: 0,
            }),
        }
    }

    #[test]
    fn the_forgebox_ca_is_accepted() {
        let ca = forgebox_ca();
        check_machine_root_candidate(&ca, &"AB".repeat(20)).unwrap();
        check_machine_root_removal(&ca).unwrap();
        let mut lan = forgebox_ca();
        lan.name_constraints.as_mut().unwrap().permitted_ip.extend([
            prefix("192.168.0.0", 16),
            prefix("10.1.0.0", 16),
            prefix("fd00::", 8),
        ]);
        check_machine_root_candidate(&lan, &"ab".repeat(20)).unwrap();
    }

    #[test]
    fn anything_else_is_refused() {
        type Change = fn(&mut CertificateInfo);
        let cases: [(&str, Change); 12] = [
            ("thumbprint", |c| c.thumbprint = "cd".repeat(20)),
            ("subject", |c| c.subject = "Evil Root CA".into()),
            ("self-signed", |c| c.self_signed = false),
            ("not a CA", |c| c.is_ca = false),
            ("no constraints", |c| c.name_constraints = None),
            ("no dns", |c| {
                c.name_constraints.as_mut().unwrap().permitted_dns.clear();
            }),
            ("public tld", |c| {
                c.name_constraints
                    .as_mut()
                    .unwrap()
                    .permitted_dns
                    .push("com".into());
            }),
            ("public dev", |c| {
                c.name_constraints
                    .as_mut()
                    .unwrap()
                    .permitted_dns
                    .push(".dev".into());
            }),
            ("empty dns", |c| {
                c.name_constraints
                    .as_mut()
                    .unwrap()
                    .permitted_dns
                    .push(String::new());
            }),
            ("public ip", |c| {
                c.name_constraints
                    .as_mut()
                    .unwrap()
                    .permitted_ip
                    .push(prefix("8.8.8.0", 24));
            }),
            ("everything ip", |c| {
                c.name_constraints
                    .as_mut()
                    .unwrap()
                    .permitted_ip
                    .push(prefix("0.0.0.0", 0));
            }),
            ("other forms", |c| {
                c.name_constraints.as_mut().unwrap().permitted_other = 1;
            }),
        ];
        for (what, change) in cases {
            let mut ca = forgebox_ca();
            change(&mut ca);
            let err = check_machine_root_candidate(&ca, &"ab".repeat(20)).unwrap_err();
            assert_eq!(err.code(), "forbidden", "{what}: {err}");
        }
        let mut odd_mask = forgebox_ca();
        odd_mask
            .name_constraints
            .as_mut()
            .unwrap()
            .permitted_ip
            .push(IpPrefix {
                addr: "127.0.0.0".parse().unwrap(),
                len: None,
            });
        assert!(check_machine_root_candidate(&odd_mask, &"ab".repeat(20)).is_err());

        let mut microsoft = forgebox_ca();
        microsoft.subject = "Microsoft Root Certificate Authority 2011".into();
        assert_eq!(
            check_machine_root_removal(&microsoft).unwrap_err().code(),
            "forbidden"
        );
    }
}

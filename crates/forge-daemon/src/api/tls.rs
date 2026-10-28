//! `tls.*`: local CA and site certificates (docs/06 §6, docs/11 §3) through the `tls`
//! actor. The CA private key never leaves the daemon; `tls.export` hands out a site's
//! own certificate and key (admin only, PEM).

use base64::Engine as _;
use forge_core::error::{ForgeError, Result};
use forge_core::platform::TrustScope;
use forge_ipc::api::methods::{
    tls_ca_export, tls_ca_install, tls_ca_rotate, tls_ca_status, tls_export, tls_issue, tls_renew,
};
use forge_ipc::api::{
    CaInstallParams, CaRotateParams, CaStatusDto, CertExportDto, CertFormat, CertificateListDto,
    JobDto, NoParams, PemDto, TlsExportParams, TlsHostParams, TlsRenewParams, TrustScopeDto,
};
use forge_ipc::server::{Ctx, Router};
use forge_tls::{LeafRequest, RenewScope};

use super::convert;
use super::route;
use crate::daemon::Daemon;

pub fn register(router: &mut Router, daemon: &Daemon) {
    route!(router, daemon, tls_ca_status, ca_status);
    route!(router, daemon, tls_ca_install, ca_install);
    route!(router, daemon, tls_ca_export, ca_export);
    route!(router, daemon, tls_ca_rotate, ca_rotate);
    route!(router, daemon, tls_issue, issue);
    route!(router, daemon, tls_renew, renew);
    route!(router, daemon, tls_export, export);
}

async fn ca_status(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<CaStatusDto> {
    Ok(convert::ca_status(&daemon.tls.status().await?))
}

async fn ca_install(daemon: Daemon, params: CaInstallParams, ctx: Ctx) -> Result<JobDto> {
    let scope = match params.scope {
        TrustScopeDto::CurrentUser => TrustScope::CurrentUser,
        TrustScopeDto::LocalMachine => TrustScope::LocalMachine,
    };
    let d = daemon.clone();
    daemon.job(&ctx, "tls.ca_install", move |_job| async move {
        d.tls.ensure_ca().await?;
        d.tls.install_ca(scope).await?;
        Ok(convert::ca_status(&d.tls.status().await?))
    })
}

async fn ca_export(daemon: Daemon, _params: NoParams, _ctx: Ctx) -> Result<PemDto> {
    Ok(PemDto {
        filename: "forgebox-ca.crt".into(),
        pem: daemon.tls.export_ca().await?,
    })
}

async fn ca_rotate(daemon: Daemon, params: CaRotateParams, ctx: Ctx) -> Result<JobDto> {
    if !params.confirm {
        return Err(ForgeError::InvalidInput(
            "rotating the CA re-issues every certificate: confirm must be true".into(),
        ));
    }
    let d = daemon.clone();
    daemon.job(&ctx, "tls.ca_rotate", move |_job| async move {
        d.tls.rotate_ca().await?;
        let _ = d.web.apply_web("ca rotated").await;
        Ok(convert::ca_status(&d.tls.status().await?))
    })
}

async fn issue(daemon: Daemon, params: TlsHostParams, ctx: Ctx) -> Result<JobDto> {
    let host = params.host.trim().to_ascii_lowercase();
    let site = daemon.sites.get_by_host(&host).await?;
    let d = daemon.clone();
    daemon.job(&ctx, format!("tls.issue {host}"), move |_job| async move {
        let request = LeafRequest {
            host: host.clone(),
            aliases: site.as_ref().map(|s| s.aliases.clone()).unwrap_or_default(),
            site_id: site.as_ref().map(|s| s.id),
        };
        let leaf = d.tls.issue(request).await?;
        Ok(convert::certificate(&leaf, site.map(|s| s.id)))
    })
}

async fn renew(daemon: Daemon, params: TlsRenewParams, ctx: Ctx) -> Result<JobDto> {
    let scope = match (&params.host, params.all) {
        (Some(host), _) => RenewScope::Host(host.trim().to_ascii_lowercase()),
        (None, true) => RenewScope::All,
        (None, false) => RenewScope::Due,
    };
    let d = daemon.clone();
    daemon.job(&ctx, "tls.renew", move |_job| async move {
        let leaves = d.tls.renew(scope).await?;
        let sites = d.sites.list().await.unwrap_or_default();
        Ok(CertificateListDto {
            certificates: leaves
                .iter()
                .map(|l| {
                    let site = sites.iter().find(|s| s.host == l.host).map(|s| s.id);
                    convert::certificate(l, site)
                })
                .collect(),
        })
    })
}

async fn export(daemon: Daemon, params: TlsExportParams, _ctx: Ctx) -> Result<CertExportDto> {
    if params.format == CertFormat::Pfx {
        return Err(ForgeError::NotImplemented(
            "PFX export of site certificates (roadmap M2-E3); use format pem".into(),
        ));
    }
    let leaf = daemon.tls.export_leaf(&params.host).await?;
    let pem = format!("{}{}", leaf.cert_pem, leaf.key_pem);
    Ok(CertExportDto {
        format: CertFormat::Pem,
        filename: format!("{}.pem", leaf.host),
        data_base64: base64::engine::general_purpose::STANDARD.encode(pem.as_bytes()),
    })
}

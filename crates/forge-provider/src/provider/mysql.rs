//! `type: mysql`: curated list (dev.mysql.com publishes md5 only and has an unstable
//! page layout, docs/05 §1). Nothing is scraped; entries are validated.

use forge_core::error::Result;
use futures::future::BoxFuture;

use super::{Provider, Release, ResolveRequest, curated, filter};

pub(crate) struct Mysql;

impl Provider for Mysql {
    fn name(&self) -> &'static str {
        "mysql"
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let releases = curated::from_manifest(req.tool, self.name(), true)?;
            filter::finish(&req, releases)
        })
    }
}

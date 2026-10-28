//! `type: postgresql-edb`: curated list (EDB publishes no index, docs/05 §1). Nothing is
//! scraped; entries are validated.

use forge_core::error::Result;
use futures::future::BoxFuture;

use super::{Provider, Release, ResolveRequest, curated, filter};

pub(crate) struct PostgresqlEdb;

impl Provider for PostgresqlEdb {
    fn name(&self) -> &'static str {
        "postgresql-edb"
    }

    fn resolve<'a>(&'a self, req: ResolveRequest<'a>) -> BoxFuture<'a, Result<Vec<Release>>> {
        Box::pin(async move {
            let releases = curated::from_manifest(req.tool, self.name(), true)?;
            filter::finish(&req, releases)
        })
    }
}

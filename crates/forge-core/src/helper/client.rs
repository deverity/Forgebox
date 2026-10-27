//! Daemon side of the helper protocol: validate, launch through [`Elevation`], check the
//! answer.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::op::{HelperOp, HelperPolicy};
use super::protocol::{HELPER_EXE_NAME, HelperRequest, HelperResponse};
use crate::error::{ForgeError, Result};
use crate::ids::RequestId;
use crate::paths::ForgeRoot;
use crate::platform::{self, Elevation};

/// Runs allowlisted operations through `forgebox-helper`. Cheap to clone.
///
/// [`HelperClient::run`] is **blocking** for as long as the UAC prompt is open plus the
/// helper's own run: call it from `spawn_blocking` with a generous timeout (ADR-0011).
#[derive(Clone)]
pub struct HelperClient {
    elevation: Arc<dyn Elevation>,
    exe: PathBuf,
    policy: Arc<HelperPolicy>,
}

impl fmt::Debug for HelperClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelperClient")
            .field("elevation", &self.elevation)
            .field("exe", &self.exe)
            .field("policy", &self.policy)
            .finish()
    }
}

impl HelperClient {
    /// Client launching `exe` through `elevation`, refusing what `policy` refuses before
    /// any prompt.
    pub fn new(
        elevation: Arc<dyn Elevation>,
        exe: impl Into<PathBuf>,
        policy: HelperPolicy,
    ) -> Self {
        Self {
            elevation,
            exe: exe.into(),
            policy: Arc::new(policy),
        }
    }

    /// The helper of `root` (`<root>/forgebox-helper.exe`, docs/08 §2) launched with the
    /// OS elevation of [`platform::elevation`].
    pub fn for_root(root: &ForgeRoot, policy: HelperPolicy) -> Self {
        Self::new(
            platform::elevation(),
            root.path().join(HELPER_EXE_NAME),
            policy,
        )
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    pub fn policy(&self) -> &HelperPolicy {
        &self.policy
    }

    /// Runs `op` with a fresh request id. Blocking.
    pub fn run(&self, op: HelperOp) -> Result<HelperResponse> {
        self.run_with_id(RequestId::new(), op)
    }

    /// Runs `op` for the API request `request_id`. Blocking.
    ///
    /// Errors: the validation error before any prompt; `needs_elevation` when the user
    /// declines the UAC prompt; the helper's refusal mapped back to its code; `timeout`
    /// or `internal` when the helper does not answer properly.
    pub fn run_with_id(&self, request_id: RequestId, op: HelperOp) -> Result<HelperResponse> {
        op.validate(&self.policy)?;
        let request = HelperRequest::with_id(request_id, self.policy.root(), op);
        let name = request.op.name();
        tracing::info!(op = name, %request_id, exe = %self.exe.display(), "running forgebox-helper");
        let outcome = self
            .elevation
            .run_helper(&self.exe, &request)
            .and_then(|response| response.into_result(&request));
        match &outcome {
            Ok(response) => {
                tracing::info!(op = name, %request_id, message = %response.message, "forgebox-helper succeeded");
            }
            Err(err) => {
                tracing::warn!(op = name, %request_id, code = err.code(), error = %err, "forgebox-helper failed");
            }
        }
        outcome
    }
}

type Answer = dyn Fn(&HelperRequest) -> Result<HelperResponse> + Send + Sync;

/// [`Elevation`] for tests: records every request and answers through a closure, without
/// any process or prompt.
pub struct FakeElevation {
    requests: Mutex<Vec<HelperRequest>>,
    answer: Box<Answer>,
}

impl fmt::Debug for FakeElevation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeElevation")
            .field("requests", &self.requests().len())
            .finish_non_exhaustive()
    }
}

impl FakeElevation {
    /// Answers every request with a success.
    pub fn accepting() -> Self {
        Self::answering(|req| Ok(HelperResponse::success(req, "fake helper: done")))
    }

    /// Behaves like a user who declines the UAC prompt.
    pub fn declining() -> Self {
        Self::answering(|_| {
            Err(ForgeError::NeedsElevation(
                "the UAC prompt was declined".to_owned(),
            ))
        })
    }

    /// Validates like the real helper (against `policy`) and answers success or the
    /// refusal, as a response document.
    pub fn validating(policy: HelperPolicy) -> Self {
        Self::answering(move |req| {
            Ok(match req.op.validate(&policy) {
                Ok(()) => HelperResponse::success(req, "fake helper: done"),
                Err(err) => HelperResponse::failure(req.request_id, &err),
            })
        })
    }

    /// Answers with `answer`.
    pub fn answering(
        answer: impl Fn(&HelperRequest) -> Result<HelperResponse> + Send + Sync + 'static,
    ) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            answer: Box::new(answer),
        }
    }

    /// Every request received so far.
    pub fn requests(&self) -> Vec<HelperRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The operations received so far.
    pub fn ops(&self) -> Vec<HelperOp> {
        self.requests().into_iter().map(|r| r.op).collect()
    }
}

impl Elevation for FakeElevation {
    fn run_helper(&self, _exe: &Path, request: &HelperRequest) -> Result<HelperResponse> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        (self.answer)(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::HostsEntry;

    fn policy() -> HelperPolicy {
        HelperPolicy::new(std::env::temp_dir().join("forgebox-root"), [".test"]).unwrap()
    }

    fn nrpt() -> HelperOp {
        HelperOp::NrptAdd {
            namespace: ".test".into(),
            nameserver: "127.0.0.1".parse().unwrap(),
        }
    }

    #[test]
    fn valid_operations_reach_the_helper() {
        let fake = Arc::new(FakeElevation::accepting());
        let client = HelperClient::new(fake.clone(), "C:/x/forgebox-helper.exe", policy());
        let id = RequestId::new();
        let response = client.run_with_id(id, nrpt()).unwrap();
        assert!(response.ok);
        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request_id, id);
        assert_eq!(requests[0].root, policy().root());
        assert_eq!(fake.ops(), vec![nrpt()]);
        assert!(format!("{client:?}").contains("forgebox-helper.exe"));
    }

    #[test]
    fn invalid_operations_never_prompt() {
        let fake = Arc::new(FakeElevation::accepting());
        let client = HelperClient::new(fake.clone(), "helper.exe", policy());
        let err = client
            .run(HelperOp::HostsSetBlock {
                entries: vec![HostsEntry::new("10.1.1.1".parse().unwrap(), "x.test")],
            })
            .unwrap_err();
        assert_eq!(err.code(), "forbidden");
        assert!(fake.requests().is_empty(), "no UAC prompt for a refused op");
    }

    #[test]
    fn declined_prompts_and_refusals_surface_as_errors() {
        let client = HelperClient::new(Arc::new(FakeElevation::declining()), "h.exe", policy());
        assert_eq!(client.run(nrpt()).unwrap_err().code(), "needs_elevation");

        let refusing = FakeElevation::answering(|req| {
            Ok(HelperResponse::failure(
                req.request_id,
                &ForgeError::VerificationFailed("bad signature".into()),
            ))
        });
        let client = HelperClient::new(Arc::new(refusing), "h.exe", policy());
        let err = client.run(nrpt()).unwrap_err();
        assert_eq!(err.code(), "verification_failed");
        assert!(err.to_string().contains("bad signature"), "{err}");

        // A stale answer for another request is not trusted.
        let stale = FakeElevation::answering(|req| {
            let other = HelperRequest::new("x", req.op.clone());
            Ok(HelperResponse::success(&other, "done"))
        });
        let client = HelperClient::new(Arc::new(stale), "h.exe", policy());
        assert_eq!(client.run(nrpt()).unwrap_err().code(), "internal");
    }

    #[test]
    fn validating_fake_answers_like_the_helper() {
        let fake = FakeElevation::validating(policy());
        let bad = HelperRequest::new(
            policy().root(),
            HelperOp::NrptAdd {
                namespace: ".com".into(),
                nameserver: "127.0.0.1".parse().unwrap(),
            },
        );
        let response = fake.run_helper(Path::new("h.exe"), &bad).unwrap();
        assert!(!response.ok);
        assert_eq!(response.code.as_deref(), Some("forbidden"));
        let good = HelperRequest::new(policy().root(), nrpt());
        assert!(fake.run_helper(Path::new("h.exe"), &good).unwrap().ok);
        assert_eq!(fake.requests().len(), 2);
    }
}

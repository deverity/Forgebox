//! `ProxySetting::System` must go through the configured proxy. Kept in its own test
//! binary (single test) because it mutates process-wide environment variables.

mod support;

use forge_core::event::EventKind;
use forge_provider::download::{DownloadRequest, ProxySetting};
use support::{Behavior, Harness, TestServer, payload};

#[tokio::test]
async fn system_proxy_setting_uses_the_configured_proxy() {
    let data = payload(10 * 1024);
    let proxy = TestServer::start().await;
    proxy.add("/sys-proxy.bin", Behavior::new(data));
    // SAFETY: this binary holds a single test and sets the variables before any
    // client (and thread reading them) is created.
    unsafe {
        std::env::set_var("HTTP_PROXY", proxy.url("/").as_str());
        std::env::set_var("http_proxy", proxy.url("/").as_str());
        std::env::set_var("NO_PROXY", "");
        std::env::set_var("no_proxy", "");
    }
    let h = Harness::new(|c| c.proxy = ProxySetting::System);
    let mut events = h.events();
    let url = url::Url::parse("http://files.forgebox-test.invalid/sys-proxy.bin").unwrap();
    let job = h
        .manager
        .download(DownloadRequest::new(vec![url], h.dest("sys")))
        .await
        .unwrap();
    let seen = h.wait_terminal(&mut events, job).await;
    assert_eq!(seen.last().unwrap().kind, EventKind::JobDone);
    assert_eq!(proxy.hits("/sys-proxy.bin").len(), 1);
}

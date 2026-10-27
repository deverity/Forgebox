//! The `hosts` managed block written by `forgebox-helper` (rendered in
//! `forge_core::helper::hosts`, which cannot depend on this crate) uses the same markers
//! and hash as every other managed block, so drift detection reads it too.

use forge_core::helper::HostsEntry;
use forge_core::helper::hosts::{apply_hosts_block, render_hosts_block};
use forge_render::managed::{CommentStyle, hash_body, parse_blocks};

fn entries() -> Vec<HostsEntry> {
    vec![
        HostsEntry::new("127.0.0.1".parse().unwrap(), "loja.test"),
        HostsEntry::new("::1".parse().unwrap(), "loja.test"),
    ]
}

#[test]
fn hosts_block_is_a_regular_managed_block() {
    let block = render_hosts_block(&entries());
    let blocks = parse_blocks(&block, CommentStyle::Hash).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].name, "hosts");
    assert!(!blocks[0].is_drifted(), "same hash algorithm");
    assert_eq!(blocks[0].recorded_hash, hash_body(&blocks[0].body));

    // Also inside a CRLF hosts file.
    let file = apply_hosts_block("# hosts\r\n127.0.0.1 localhost\r\n", &entries()).unwrap();
    let blocks = parse_blocks(&file, CommentStyle::Hash).unwrap();
    assert_eq!(blocks.len(), 1);
    assert!(!blocks[0].is_drifted());

    let edited = file.replace("::1 loja.test", "::1 loja.test extra.test");
    assert!(parse_blocks(&edited, CommentStyle::Hash).unwrap()[0].is_drifted());
}

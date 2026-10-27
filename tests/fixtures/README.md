# Fixtures

- `registry/`: a minimal registry (2 tools + 1 stack) whose `index.yaml` is signed with the
  development key (`registry/keys/dev.pub`). Regenerate after editing a manifest with
  `cargo xtask registry-index --dir tests/fixtures/registry --name fixture` and
  `cargo xtask registry-sign --key <dev.key> --dir tests/fixtures/registry`.
- `registry-bad-sha/`: the same registry with a wrong sha256 for `hello-db` in the index
  (validly signed). The loader must refuse it.

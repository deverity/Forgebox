-- 0002_registry_loader: columns the registry loader (forge-registry) needs per registry
-- (M1-E1-T3, docs/05 §7, docs/11 §2). Published migrations are never edited.

-- Dev only: an unsigned index may load, shown as "not verified".
ALTER TABLE registries ADD COLUMN trusted_unsigned INTEGER NOT NULL DEFAULT 0
    CHECK (trusted_unsigned IN (0, 1));
-- Minisign public key (base64 `RW...`) that must have signed the index.
ALTER TABLE registries ADD COLUMN public_key TEXT;
-- How long the cache is used without asking the source again.
ALTER TABLE registries ADD COLUMN ttl_secs INTEGER NOT NULL DEFAULT 3600 CHECK (ttl_secs >= 0);
-- Result of the last successful load: 'verified' (minisign) or 'unverified'.
ALTER TABLE registries ADD COLUMN verification TEXT
    CHECK (verification IN ('verified', 'unverified'));
-- SHA-256 of the index.yaml bytes in the cache.
ALTER TABLE registries ADD COLUMN index_sha256 TEXT;
-- Message of the last failed refresh, NULL after a success.
ALTER TABLE registries ADD COLUMN last_error TEXT;

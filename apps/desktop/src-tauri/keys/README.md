# Updater keys

`updater-dev.pub` is the minisign public key (Tauri format: base64 of the `.pub` file) that debug
builds trust when `FORGEBOX_UPDATER_ENDPOINT` points the updater at a local test server
(ADR-0038). Its private half was discarded on purpose: tests generate throw-away key pairs and
inject the public half, so no secret is committed. Release builds never use this key; the
production key and endpoint arrive with M4-E1.

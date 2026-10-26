# ADRs — Architecture Decision Records

Formato: contexto → decisão → alternativas rejeitadas → consequências. Um arquivo por decisão, numerado, nunca editado após aceito (cria-se um novo que o substitui).

| # | Decisão | Status |
|---|---|---|
| [0001](0001-rust-user-space-daemon.md) | Motor em Rust como daemon user-space, sem Windows Services | aceito |
| [0002](0002-gui-tauri.md) | GUI com Tauri 2 + WebView2 (não Electron, não GUI Rust pura) | aceito |
| [0003](0003-php-fastcgi-pool-manager.md) | Pool FastCGI próprio por versão de PHP (não mod_fcgid/mod_php) | aceito |
| [0004](0004-dns-nrpt-resolver.md) | DNS wildcard via resolver próprio + NRPT; hosts só como fallback | aceito |
| [0005](0005-current-junction-switching.md) | Troca de versão ativa por junction `current` sem alterar env-vars | aceito |
| [0006](0006-yaml-manifests-registry.md) | Catálogo de tools como manifestos YAML em registry assinado | aceito |
| [0007](0007-sqlite-state-etc-derived.md) | SQLite como fonte de verdade; `etc/` é derivado com blocos gerenciados | aceito |
| [0008](0008-single-root-folder-layout.md) | Layout de pasta única (`C:\Forgebox`) e instalação sob demanda | aceito |
| [0009](0009-remote-ready-ipc.md) | API "remote-ready" desde a v1 (transporte abstrato, ULIDs, streams com seq, perfis) | aceito |
| [0010](0010-case-sensitive-by-default.md) | Case-sensitive por padrão via NTFS por diretório (paridade Linux), desativável | aceito |
| [0011](0011-actor-concurrency-model.md) | Concorrência por atores com tokio; nada bloqueia o controlador nem um serviço bloqueia outro | aceito |
| [0012](0012-yaml-crate-serde-yaml-ng.md) | YAML com `serde_yaml_ng` (`serde_yml` deprecado), atrás de `forge_manifest::yaml` | aceito |
| [0013](0013-jiff-timestamps.md) | Timestamps UTC com `jiff::Timestamp` (re-export `forge_core::Timestamp`), RFC 3339 no fio | aceito |

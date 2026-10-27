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
| [0014](0014-concurrency-skeleton.md) | Esqueleto de concorrência: supervisão no task do ator, travas ordenadas, writer SQLite em lotes com `SAVEPOINT` | aceito |
| [0015](0015-ipc-protocol.md) | Protocolo IPC: JSON-RPC 2.0 em NDJSON, `request_id` obrigatório, `system.hello` com token, tabela única de métodos, streams com fila drop-oldest e `resume_from` | aceito |
| [0016](0016-ctrl-break-helper-process.md) | `CTRL_BREAK` entregue por processo auxiliar (`forgebox-ctrl.exe`), nunca trocando o console do daemon | aceito |
| [0017](0017-verify-extract-stack.md) | Verificação e extração em Rust puro (rpgp, minisign-verify, zip 9, sevenz-rust2, lzma-rust2), Authenticode sem revogação, MSRV 1.88 | aceito |
| [0018](0018-detached-index-signature.md) | Índice do registry assinado por minisign destacado (`index.yaml.minisig`) sobre os bytes crus, sem canonicalização | aceito |
| [0019](0019-instance-lifecycle-health-restart-metrics.md) | Ciclo de vida da Instance: máquina de estados pura, `Stopping` com causa, `Failed` estruturado, health com jitter, restart com backoff na janela, métricas por Job com CPU % da máquina | aceito |
| [0020](0020-prechecks-ports-vcredist.md) | Pré-checagens no início de `Starting` (runtime, `depends_on`, portas, config) e validação antes de restart; VC++ por registro + versão de arquivo (vs18 ≥ 14.50); alocador de portas como operação única no writer do SQLite | aceito |
| [0021](0021-php-pool-actor-request-estimation-blue-green.md) | PhpPool: workers como instâncias do supervisor (Job aninhado, barramento privado), requests estimadas pela tabela TCP (`TIME_WAIT`), recycle proativo com `PHP_FCGI_MAX_REQUESTS` como teto, drenagem atrelada ao listener de upstream, blue/green com standby | aceito |
| [0022](0022-elevated-helper-protocol.md) | Protocolo do `forgebox-helper`: allowlist tipada validada no daemon e no helper, JSON versionado, troca por arquivos com DACL em `state/locks/`, NRPT/`hosts` só sob sufixos de uso local | aceito |
| [0023](0023-local-ca-name-constraints-trust.md) | CA local ECDSA P-256 com name constraints críticas (TLDs de uso local, `localhost`, loopback), `pathLen 0`, chaves só do usuário, confiança por thumbprint SHA-1 com livro `trust.json`, verificação pelo CryptoAPI com raiz exclusiva, rotação em duas fases com confiança primeiro, CRL vazia servida em `127.0.0.1` para o Schannel (`curl.exe`) | aceito |
| [0024](0024-helper-execution.md) | Execução das operações do `forgebox-helper`: WMI/COM/CryptoAPI sem shell, validação léxica + canônica, arquivos da raiz sem seguir links, instalador VC++ copiado e verificado antes de rodar, só CA Forgebox com Name Constraints em `LocalMachine\Root`, log antes de agir, NRPT local com GPO → `hosts` | aceito |
| [0025](0025-layout-staging-shims-user-path.md) | Layout de versões: `post_install` na cópia em staging (variáveis física/final), turno por tool, `forge use` por seleção + junction, shims `.cmd` com tabela de aliases gerada (`version_files`, `project_key`, `interpreter` no schema), PATH do usuário no início, sem duplicatas, `REG_EXPAND_SZ` preservado e `WM_SETTINGCHANGE` com timeout | aceito |
| [0027](0027-update-engine-active-version-rollback-retention.md) | Motor de updates: ator por `(tool, série)`, linha de update (série + variantes escolhidas), versão ativa pelo histórico, rollback que fixa a série, retenção só depois da troca, colaboradores por trait (`PoolSwitcher`, `ReleaseSource`, `ArchiveFetcher`, `ToolCatalog`, `MeteredProbe`) | aceito |

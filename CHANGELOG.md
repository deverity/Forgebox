# Changelog

Todas as mudanças relevantes do Forgebox são registradas aqui.

O formato segue o [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e o projeto adota [Versionamento Semântico](https://semver.org/lang/pt-BR/).

O detalhe por tarefa está em [docs/STATUS.md](docs/STATUS.md); as decisões de arquitetura, em [docs/adr/](docs/adr/) (as ADRs 0012 a 0033 foram tomadas nesta versão).

## [Não lançado]

## [0.1.0] — 2026-10-27

Primeira versão: fundação do repositório (M0) e motor sem interface gráfica com a CLI `forge` (M1). Ainda não há GUI nem instalador (M2 e M4).

### Adicionado

#### Ferramentas e instalação
- Manifestos YAML schema v1 com 5 kinds (`deny_unknown_fields`, erros com linha e coluna), lint do registry com 7 regras e JSON Schemas gerados (ADR-0012).
- Registry com fontes `Dir`/`Http`/`Git`, índice assinado com minisign destacado, sha256 por manifesto, cache com TTL/ETag e política de terceiros (ADR-0018). Catálogo inicial: 12 tools/extensões e 4 stacks.
- `discover`: reconhece versões já instaladas (inclusive as do Laragon) a partir de `spec.discover`, sem lógica por tool.
- Download engine com retomada (Range), mirrors, prioridade, limite de banda, proxy, cancelamento e progresso (velocidade e ETA).
- Verificação obrigatória (sha256/sha512, PGP, minisign, Authenticode) e extração segura (zip, 7z, tar.gz, tar.xz) com proteção contra bombas e caminhos maliciosos, em Rust puro (ADR-0017; MSRV 1.88).
- Providers: php-windows, github-release, http, nodejs-dist, mariadb, apache-lounge, nginx-org, mysql e postgresql-edb.
- Layout multi-versão `bin/<tool>/<versão>` com junction `current` (troca em ~5 ms), instalação atômica via staging, shims `.cmd` com `.php-version`/`.nvmrc`/`.node-version`/`forgebox.yaml` e PATH do usuário (ADR-0025).
- Motor de updates com políticas, instalação lado a lado, migração, rollback que fixa a série e retenção de versões (ADR-0027).

#### Serviços e supervisor
- Supervisor baseado em Job Objects aninhados: spawn, parada em cascata (`CTRL_BREAK` via `forgebox-ctrl.exe`, ADR-0016), logs rotativos (10 MiB x 5) e nenhum Windows Service.
- Máquina de estados da instância com health probes (tcp/http/command/pid), políticas de restart com backoff e métricas por Job (CPU, memória, handles, uptime) (ADR-0019).
- Pré-checagens ao iniciar: conflito de portas (tabela TCP, com o processo dono), VC++ Redistributable por sabor e alocador de portas persistente (ADR-0020).

#### PHP pools
- `PhpPool` com php-cgi/FastCGI por versão e por site, warmup obrigatório, reciclagem proativa, escala por conexões e troca blue/green sem derrubar requisições (ADR-0021).

#### Sites, render e drivers
- Templates de servidor web (nginx, Apache, Caddy) e de php, mysql, mariadb, postgresql e mailpit, com blocos gerenciados `# forgebox:begin/end`, detecção de drift e access log JSON `forgebox`. nginx e Apache validados com binários reais.
- 9 drivers de framework com desempate por confiança, sites parked com watcher, `forgebox.yaml` por projeto, proxies (WebSocket por padrão) e processes de sites (`command`/`runtime`, `$PORT`, lançamento `on-visit`) (ADR-0028).

#### DNS e TLS
- Servidor DNS local (hickory) e configuração por NRPT; arquivo hosts gerenciado pelo helper.
- CA local ECDSA P-256 com name constraints críticas (somente TLDs de uso local), certificados por site (825 dias), chaves com DACL só do usuário, confiança por thumbprint em `CurrentUser\Root` e rotação em duas fases (ADR-0023).
- Bundle de CAs (`certs/ca-bundle.pem` = CA local + raízes do Windows, sem as desconfiadas) para Composer/curl em redes com inspeção TLS (ADR-0032).

#### Paridade Linux / case-sensitivity
- Diretórios do Forgebox criados com a flag NTFS case-sensitive; configs geradas com regex case-sensitive, LF, UTF-8 sem BOM (ADR-0010).
- Conversão de árvores (liga/desliga) recriando as pastas, com verificação por contagem, bytes e hash, desfazer em caso de falha e recuperação após queda; Case Mismatch Finder (ADR-0026).
- MySQL 8.4 e MariaDB 11.8 validados com `lower_case_table_names=0` em NTFS case-sensitive.

#### Daemon, IPC e CLI
- `forgeboxd` em espaço de usuário: instância única por raiz, boot com prazo por passo, autostart em ondas por `depends_on`, scheduler com jitter, shutdown em cascata e logs JSON diários (ADR-0029).
- API JSON-RPC 2.0 sobre NDJSON, pronta para modo remoto: `request_id`, idempotência, handshake por token, named pipe com DACL, streams com `seq`, `resume_from` e `*.lagged` (ADR-0009, ADR-0015).
- Modelo de concorrência por atores com timeout obrigatório, jobs com cancelamento e writer SQLite dedicado em lotes (ADR-0011, ADR-0014); orçamentos de latência verificados contra o daemon real.
- CLI `forge` com todos os grupos de comandos de docs/09, `--json` igual ao DTO da API, códigos de saída estáveis, daemon iniciado automaticamente, `doctor`, completions, mensagens pt-BR/en-US, `exec`/`env`/`path` e servidor MCP (ADR-0030).

#### Helper elevado
- `forgebox-helper` com allowlist tipada de operações (NRPT, hosts, `LocalMachine\Root`, firewall, Defender, VC++), protocolo versionado por arquivos em `state/locks/`, validação léxica e canônica, `--dry-run` e log JSON antes de agir; elevação somente por UAC e só quando necessária (ADR-0022, ADR-0024).

#### Import do Laragon
- Varredura somente leitura de `usr/laragon.ini`, `bin/*`, vhosts Apache/nginx e `data/`; `www/` vira parked; relatório de importados e pulados com motivo; dry-run pelo mesmo caminho da importação real; idempotente (ADR-0031).

#### CI e infra
- Workspace Cargo (16 crates + `xtask`), `xtask` com `schema`, `gen-ts`, `lint-registry` e `registry-index`, CI no GitHub Actions, dependabot e `scripts/ci.ps1` (build, fmt, clippy, test, cargo deny).
- Gate M1: `scripts/smoke.ps1` (Laravel de ponta a ponta com PHP, nginx, MySQL, Mailpit e Composer) e testes de concorrência contra o daemon real (ADR-0032, docs/16).

### Corrigido
- DNS em máquinas de domínio: `forge dns enable` dizia "aplicada" e `loja.test` não resolvia quando a política de grupo deixa a chave NRPT de política **vazia** (o Windows ignora então todas as regras locais). Agora a chave conta mesmo vazia, toda regra é verificada pelo resolvedor do Windows com um nome de sonda antes de valer e, se não funcionar (também com VPN ou agente de segurança interceptando o DNS), o Forgebox usa o arquivo `hosts`, que acompanha sites e aliases, inclusive depois de reiniciar o daemon (o modo fica em `state/dns-policy.json`, e só ele arma o modo: um bloco de outra raiz no `hosts` nunca é adotado nem reescrito sozinho); quando a NRPT volta a funcionar, `forge dns enable` tira o bloco; rodar `forge dns enable` de novo não pede UAC se nada mudou, e `forge dns disable` nunca deixa o bloco para trás. `forge dns status`, `forge dns test` e `forge doctor` mostram o modo efetivo (`nrpt` ou `hosts`) e o motivo (ADR-0033).
- Correções de integração encontradas pelo gate M1: instalação de bancos com `instance.*` em `post_install`; faixas de porta de `forge config set` aplicadas sem restart; pasta de logs por instância; `forge exec` com `PHPRC`/extensões e `PATHEXT`; shims com ambiente (`forgebox-env.cmd`); `bcmath` embutido; pool PHP com versão `8.4.27+1`; Composer confiando na CA local e na raiz corporativa.

### Problemas conhecidos
- Não existe `forge tls ca uninstall`: o smoke interativo deixa a CA do Forgebox em `CurrentUser\Root`. Para remover: `Get-ChildItem Cert:\CurrentUser\Root | Where-Object Subject -like '*Forgebox*' | Remove-Item`.
- `forge new` chega em M3-E2; `db.*` e `mail.*` em M2-E3 (o smoke usa `forge exec` com o cliente oficial; esses comandos respondem `not_implemented` com a tarefa). Também sem implementação: `share`, `stats`/`metrics`, `remote`, `bundle`, `backup`, `self-update` e `lan`.
- `bin/php/current` direto no PATH roda sem `PHPRC`; use os shims ou `forge exec`.
- O probe TCP no Windows mantém o serviço em `Starting` por ~0,5 s (docs/03 §7.6).
- Falso positivo do antivírus: o heurístico de ML do Windows Defender já classificou o `forge.exe` de debug (sem assinatura) como `Trojan:Win32/Bearfoos.A!ml` e o pôs em quarentena. Recompile e, de preferência, rode o smoke com `-Root` fora de `%TEMP%` (ver docs/16). Os binários de release serão assinados em M4.
- Ainda não há interface gráfica (GUI em M2), instalador nem build portátil (M4).

### Verificação
- Gate M1: `scripts/smoke.ps1 -NonInteractive` passa de ponta a ponta (ver docs/16).
- Smoke interativo (UAC do `forge dns enable` e diálogo de `forge tls ca install`): <!-- interactive-smoke -->

# 05 — Providers de download e auto-update

## 1. Providers nativos (`forge-provider::provider::*`)

Um provider resolve `(tool, constraint, variants) → Vec<Release{version, url, sha256, size, variants, published_at}>`. Manifestos escolhem o provider por `type`. Providers nativos existem porque alguns upstreams têm APIs/índices próprios que valem a pena parsear com cuidado.

| type | Upstream | Como resolve | Checksum | Observações |
|---|---|---|---|---|
| `php-windows` | windows.php.net | `GET https://downloads.php.net/~windows/releases/releases.json` → chaves `"8.4"`, `"8.3"`… cada uma com `version` e variantes `ts-vs17-x64`, `nts-vs17-x64`, `ts-vs16-x64`… contendo `zip.path`, `zip.size`, `zip.sha256`, `debug_pack`, `devel_pack`. Versões anteriores em `releases/archives/` (listagem HTML parseada; sha256 via `.sha256` quando existir) | sha256 oficial | Builds de QA (`qa/`) só no canal preview. Observado em 26/10/2026: 7.4 (vc15), 8.0–8.3 (vs16), 8.4–8.5 (vs17) |
| `apache-lounge` | apachelounge.com | Parse das páginas `download/VS17/` e `download/VS18/`, arquivos `httpd-2.4.NN-YYMMDD-win64-VS1x.zip`; `.txt` com sha; mod_fcgid e outros módulos na mesma página | sha256/sha512 publicados | Site sem API; cache agressivo; fallback para `releases:` estático no manifesto |
| `nginx-org` | nginx.org | Parse de `nginx.org/en/download.html` (mainline/stable) → `nginx-1.NN.N.zip` | sem checksum upstream (só PGP) → verificamos PGP com chave embutida | |
| `github-release` | api.github.com | `releases` + filtro por regex de asset; rate-limit tratado com ETag/cache; token opcional | `*.sha256`/`checksums.txt` quando existir | Usado por caddy, frankenphp, mailpit, minio, meilisearch, cloudflared, composer, bun, mongosh… |
| `mysql` | dev.mysql.com | Parse de `downloads/mysql/` (zip "Windows (x86, 64-bit), ZIP Archive") e `archives/`; md5 publicado → **exigimos sha256** via `downloads.mysql.com/archives/get/p/23/file/...` + hash calculado e fixado no registry (`releases:` curado) | sha256 curado | API instável → lista curada no registry, atualizada por bot |
| `mariadb` | mariadb.org / archive.mariadb.org | `https://downloads.mariadb.org/rest-api/mariadb/<series>/` (REST oficial) → `winx64-packages.zip` com sha256 | sha256 oficial | API REST estável |
| `postgresql-edb` | enterprisedb.com | Lista curada (`postgresql-<ver>-windows-x64-binaries.zip`); sha256 curado | sha256 curado | EDB não publica índice; bot do registry atualiza |
| `nodejs-dist` | nodejs.org/dist | `index.json` oficial (versões, lts, files) + `SHASUMS256.txt` por versão → `node-vX-win-x64.zip` | sha256 oficial | Canais `lts`, `current` |
| `python-embed` | python.org | `python-3.X.Y-embed-amd64.zip` + também "nuget" `python.3.X.Y.nupkg` (contém pip) | sha256 (via `/ftp/python/3.X.Y/` listagem md5 → curado) | |
| `http` | genérico | URL com placeholders `{version}`, `{variant.*}`; versões via `releases:` estático ou `version_source: {url, regex|json_path}` | obrigatório | Para tudo que não tem API |
| `local` | arquivo local/zip | `forge tool add-local <zip>` com manifesto embutido | sha256 calculado e exibido | |
| `wsl` / `docker` *(v2)* | WSL2 / Docker Desktop | Para tools sem build Windows (Valkey, DragonflyDB, KeyDB, RabbitMQ) | imagem digest | Ver [12-stacks-compat.md](12-stacks-compat.md) |

Todos os providers:
- Cacheiam metadados em `state/registry/cache/<provider>/` com TTL e `ETag`.
- Respeitam `prefs.proxy` (sistema ou manual) e `prefs.offline` (só cache).
- Expõem `published_at` para a UI mostrar "lançado há 3 dias".
- Devolvem **todas** as variantes para que a UI mostre NTS/TS, VS16/VS17 com explicação ("NTS é o recomendado para FastCGI").

## 2. Download
Implementação: `forge-provider::download` (`DownloadManager`, ator da fila; cada download é um job de `forge_core::jobs`).
- API: `download(DownloadRequest { urls, expected_size, expected_sha256, dest, priority }) -> JobId`. Resultado em `job.done`: `{path, size, sha256, source, mirror_failures[]}` (`sha256` é o calculado). Erros de pedido (sem URL, esquema não http/https, sha256 que não tem 64 hex) e `prefs.offline` falham na hora, sem criar job (offline = `conflict`).
- Fila com prioridade (`UserAction` > `AutoUpdate`, FIFO dentro da prioridade), paralelismo 2 downloads × 4 conexões. Cancelar um job ainda na fila não ocupa vaga. O mesmo arquivo pedido de novo para o mesmo destino devolve o job existente; outro destino é `conflict` (um `.part` por arquivo).
- Retomada por `Range`; arquivo parcial em `tmp/downloads/<chave>.part`, onde `<chave>` são os 16 primeiros hex do sha256 esperado (ou do sha256 da primeira URL, sem sha). Ao retomar, o `.part` é re-hasheado (`spawn_blocking`); se a verificação final falhar após uma retomada, o parcial é descartado e o download recomeça uma vez antes de culpar o mirror. Servidor que ignora `Range` (200) reinicia do zero.
- Arquivos ≥ 50 MB cujo servidor anuncia `Accept-Ranges: bytes` usam até 4 conexões `Range` escrevendo cada uma sua fatia do `.part` pré-alocado; as posições ficam em `<chave>.part.json` para retomar só o que falta. O sha256 desses é calculado no fim (streaming só no modo de uma conexão). Buffers de 1 MiB; escrita e hash sempre em `spawn_blocking`.
- Progresso: `job.progress` a cada 250 ms com `bytes_done`, `bytes_total`, `pct`, `speed_bps` (EMA, alfa 0,3) e `eta_secs` (campos `speed_bps`/`eta_secs` foram acrescentados a `JobProgress`); a mensagem traz a fonte ("Baixando de downloads.php.net").
- Falhas transitórias (conexão cortada, timeout, 408/429/502/503/504) retomam no mesmo mirror até 3 vezes (espera 1 s × tentativa); outros erros HTTP passam ao mirror seguinte. Cada falha de mirror é registrada com o motivo (`mirror_failures`) e entra na mensagem de erro final (`io`, ou `verification_failed` se todos serviram bytes inválidos).
- Limite de banda opcional (token bucket compartilhado, rajada de 1 s, alterável em execução); pausa em conexão medida (Windows `NetworkCostManager`) fica para o scheduler de updates (M1-E2-T5).
- Proxy: sistema por padrão (reqwest), `Direct` ou manual (`DownloadConfig.proxy`, vindo de `prefs.proxy`).
- Cancelamento (`job.cancel`): mantém o `.part` e não emite `job.done`.

## 3. Verificação
1. Tamanho (quando conhecido).
2. SHA-256 (ou SHA-512). Obrigatório. Sem hash → só com `--allow-unverified` e aviso persistente no card.
3. Assinatura quando disponível: PGP (nginx, PostgreSQL via EDB não; Node via SHASUMS256.txt.sig; Composer via `.sig`), minisign (registry e o próprio Forgebox).
4. Authenticode do `.exe`/`.dll` principal (quando assinado pelo upstream: Memurai, HeidiSQL, cloudflared) → exibimos o publisher.

## 4. Políticas de atualização (por tool, sobrescrevível por versão/instância)

| Política | Comportamento |
|---|---|
| `manual` | Nunca checa. Usuário clica "Verificar". |
| `notify` | Checa; mostra badge e notificação; nada é baixado. |
| `auto-patch` *(padrão para runtimes e web servers)* | Baixa e instala patch dentro da mesma série (8.3.36 → 8.3.37); troca pools blue/green; mantém `retention` versões. |
| `auto-minor` | Também segue minor (1.28 → 1.30 do Nginx). Nunca cruza major. |
| `auto` | Tudo, inclusive major. Só para tools sem estado (cloudflared, composer). |

Regras:
- **Bancos de dados nunca são auto-atualizados** (dados em `data/` exigem upgrade de schema; `mysql_upgrade`/`pg_upgrade`). Política fixa `notify` com assistente de upgrade manual que faz backup antes.
- O usuário escolhe janela (`quiet_hours`) e se quer updates em conexão medida.
- Toda atualização gera entrada no **histórico** com botão **Rollback** (re-aponta pools/`current` para a versão retida; nunca apaga a anterior antes da nova passar no health).
- Para PHP: ao atualizar dentro da série, migra `ext/` extra (Xdebug etc.) e re-renderiza `conf.d/`. Se uma extensão não for compatível com o novo build (ex. mudança vs16 → vs17 numa série, o que não acontece dentro de uma série), a atualização é bloqueada com explicação.

## 5. Atualização do próprio Forgebox
- `tauri-plugin-updater` com `latest.json` em `https://github.com/forgebox/forgebox/releases/latest/download/latest.json`, assinado com chave minisign do projeto; modo `passive` no Windows.
- O daemon tem seu próprio canal: a GUI atualizada pede ao daemon para se auto-substituir (`forgeboxd --self-update <path>`): valida assinatura, para pools e serviços de forma graciosa **ou** usa handoff (novo daemon herda Job Objects via `DuplicateHandle` — v1.1), troca binário, reinicia.
- Canais: `stable`, `beta`. Changelog renderizado na UI antes de aplicar.

## 6. Bot do registry
GitHub Action (`registry/.github/workflows/bump.yml`) roda a cada 6 h: executa cada provider, detecta novas versões, calcula sha256 para upstreams sem hash, abre PR com a lista `releases:` atualizada e mudanças de `latest`. Humano aprova (ou auto-merge para patch). Assim o catálogo nunca depende de "alguém lembrar".

## 7. Offline e ambientes corporativos
- `forge bundle export php@8.3 nginx mysql@8.4 -o stack.fbx` gera um pacote (zip + manifestos + hashes) para instalar em máquinas sem internet (`forge bundle import`).
- Proxy corporativo: usa configuração do sistema (WinHTTP) por padrão; permite CA corporativa extra (`prefs.extra_ca_bundle`) para os downloads.
- Registry espelhado interno: `forge registry add file://\\servidor\forgebox\index.yaml`.

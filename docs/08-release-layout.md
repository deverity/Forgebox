# 08 — Layout do release para o cliente final

Define exatamente o que o usuário recebe e como fica o disco depois de instalar e usar. Princípio herdado do Laragon e levado ao extremo: **uma pasta raiz, tudo dentro, nada escondido em `%APPDATA%` ou no registro além do mínimo**.

## 1. Artefatos publicados por release

| Artefato | Conteúdo | Tamanho alvo |
|---|---|---|
| `Forgebox-1.0.0-x64-setup.exe` | NSIS; instala para o usuário atual (sem admin) em `%LOCALAPPDATA%\Programs\Forgebox` **ou** modo "pasta única" em `C:\Forgebox` (padrão recomendado) | ≤ 25 MB |
| `Forgebox-1.0.0-x64-portable.zip` | Mesmo conteúdo, extrai e roda; cria `portable.flag` | ≤ 25 MB |
| `forge-1.0.0-x64.zip` | Só CLI + daemon (headless, para CI/servidores de dev) | ≤ 12 MB |
| `latest.json` + `*.sig` | Manifesto do updater (assinado minisign) | — |
| `SHA256SUMS.txt` + `.sig` | Hashes de tudo | — |
| `registry-snapshot.tar.zst` | Snapshot do catálogo para instalação offline | ~1 MB |

Nenhum artefato embute PHP/Nginx/MySQL. O instalador baixa o que o usuário escolhe no onboarding. Alternativa opcional: `Forgebox-1.0.0-x64-bundle-laravel.exe` (instalador + snapshot de uma stack para ambientes sem internet), gerado sob demanda pelo `forge bundle`.

## 2. Árvore instalada (modo pasta única, `C:\Forgebox`)

```
C:\Forgebox\
├── Forgebox.exe                  # GUI (Tauri). ~8 MB
├── forgeboxd.exe                 # daemon (motor). ~7 MB
├── forge.exe                     # CLI. ~4 MB
├── forgebox-helper.exe           # helper elevado (manifest requireAdministrator; só roda sob demanda)
├── forgebox-ctrl.exe             # auxiliar interno do daemon, sem elevação: entrega CTRL_BREAK a um serviço (ADR-0016)
├── forge-mcp.exe                 # servidor MCP (opcional)
├── uninstall.exe                 # NSIS
├── portable.flag                 # presente só no portátil: força tudo relativo e sem registro/PATH
├── VERSION                       # "1.0.0+build.sha"
│
├── app\                          # recursos da GUI que não vão embutidos (ícones de tools, templates de UI)
│   └── icons\tools\*.svg
│
├── bin\                          # TOOLS INSTALADAS — layout versionado, visível, copiável
│   ├── shims\                    # php.cmd, composer.cmd, node.cmd... → versão do projeto (.php-version, .nvmrc, forgebox.yaml) ou bin\<tool>\current; forgebox-project.ps1 lê o forgebox.yaml (opcional ao PATH; docs/06 §2.3)
│   ├── php\
│   │   ├── 8.3.37-nts-vs17-x64\   # pasta = versão + variantes (nome canônico do Forgebox)
│   │   ├── 8.4.14-nts-vs17-x64\
│   │   ├── 7.4.33-nts-vc15-x64\
│   │   └── current  → 8.4.14-nts-vs17-x64   (junction; CLI padrão)
│   ├── composer\2.10.3\ , composer\current
│   ├── node\20.19.0\ , node\22.12.0\ , node\current
│   ├── apache\2.4.69-vs18-x64\
│   ├── nginx\1.30.6\
│   ├── caddy\2.11.2\
│   ├── mysql\8.4.6\ , mysql\9.4.0\
│   ├── mariadb\11.8.3\
│   ├── postgresql\17.6\
│   ├── memurai\4.1.6\
│   ├── mailpit\1.28.0\
│   ├── minio\2026-09-30\
│   ├── git\2.51.0\ , git\current
│   ├── cloudflared\2026.10.1\
│   ├── heidisql\12.12\
│   └── phpmyadmin\5.2.3\ , adminer\5.3.0\      # web apps: servidos em forgebox.test/phpmyadmin
│
├── etc\                          # CONFIGURAÇÃO RENDERIZADA (editável; blocos gerenciados)
│   ├── forgebox.toml             # preferências globais (raiz, TLD, portas, políticas, editor…)
│   ├── templates\                # overrides do usuário (mesmo caminho dos embutidos) — vazio por padrão
│   ├── php\
│   │   ├── 8.3\php.ini , 8.3\conf.d\10-forgebox.ini , 20-xdebug.ini
│   │   └── 8.4\...
│   ├── nginx\
│   │   ├── nginx.conf            # inclui conf.d\*, upstreams\*, sites\*
│   │   ├── conf.d\ , snippets\ , upstreams\php_8_3.conf , sites\loja.test.conf
│   ├── apache\                   # httpd.conf + mesma estrutura
│   ├── caddy\Caddyfile
│   ├── mysql\default\my.ini , mysql\legacy\my.ini
│   ├── mariadb\default\my.ini
│   ├── postgresql\default\postgresql.conf , pg_hba.conf
│   ├── mailpit\default.args
│   └── sites\                    # espelho YAML legível de cada site (fonte de verdade é o SQLite; aqui é export para git/backup)
│       └── loja.test.yaml
│
├── data\                         # DADOS DE INSTÂNCIAS (nunca tocados por updates)
│   ├── mysql\default\ , mysql\legacy\
│   ├── mariadb\default\
│   ├── postgresql\default\
│   ├── memurai\default\dump.rdb
│   ├── mailpit\default\mailpit.db
│   ├── minio\default\
│   └── node\22.12.0\npm-global\  # pacotes globais por versão
│
├── www\                          # diretório "parked" padrão (cada subpasta vira <nome>.test) — NTFS case-sensitive (flag herdada pelos projetos)
│   ├── loja\
│   └── blog\
│
├── certs\
│   ├── ca\forgebox-ca.crt , forgebox-ca.key (ACL: só o usuário)
│   │      forgebox-ca.crl                      # CRL vazia assinada pela CA (servida em 127.0.0.1:47480, ADR-0023)
│   │      trust.json                           # thumbprints SHA-1 das raízes que o Forgebox pôs no Windows (remoção limpa)
│   ├── sites\loja.test.crt , loja.test.key (ACL: só o usuário) , ...
│   └── ca-bundle.pem                         # CA local + raízes confiadas pelo Windows, para PHP/curl/Composer (docs/06 §6.5)
│
├── logs\
│   ├── forgeboxd.log.2026-10-27 ...          # daemon (tracing, JSON lines; um arquivo por dia, últimos 14)
│   ├── helper.log                             # tudo que o helper elevado fez (JSON lines: ts, request_id, op, dry_run, ok, code, message)
│   ├── nginx\access.json , error.log
│   ├── apache\...
│   ├── php\8.3\error.log , 8.3\pool.log
│   ├── mysql\default\error.log
│   ├── mysql\default.out.log , default.out.log.1 … .4   # stdout/stderr capturados pelo supervisor (10 MiB × 5)
│   └── apps\loja.test\web.log , queue.log
│
├── state\                        # ESTADO INTERNO (não editar à mão)
│   ├── forgebox.db               # SQLite: tools, versões, instâncias, sites, portas, eventos, histórico de updates
│   ├── ipc.token                 # token da API local (ACL: só o usuário)
│   ├── dns-policy.json           # TLDs em modo hosts (fallback da NRPT, ADR-0033); gravado por dns enable/disable
│   ├── registry\                 # cache do(s) registry(ies) + índices de providers
│   └── locks\                    # forgeboxd.lock (pid, versão e pipe do daemon em execução; removido na parada) + arquivos de troca do helper
│
├── tmp\                          # efêmero (limpo automaticamente)
│   ├── downloads\ , stage\ , sessions\php\8.3\ , uploads\
│
├── backups\                      # dumps agendados/manuais de bancos, snapshots de etc\ antes de updates
│   └── 2026-10-26T13-00\mysql-default-loja.sql.zst
│
└── manifests\
    └── local\                    # manifestos do usuário (kind: Tool/Extension) fora do registry
```

### 2.1 Modo "por usuário" (alternativo)
Se o usuário escolher instalar em `%LOCALAPPDATA%\Programs\Forgebox`, os binários ficam lá e **a raiz de dados** (`bin\ etc\ data\ www\ certs\ logs\ state\ tmp\ backups\ manifests\`) vai para a pasta escolhida (padrão `C:\Forgebox`). `forgebox.toml` em `%LOCALAPPDATA%\Forgebox\root.toml` aponta a raiz. Tudo o resto é idêntico. **Exceção pendente (M4-E1):** o `forgebox-helper.exe` só atende a raiz em que ele mesmo está ([ADR-0022](adr/0022-elevated-helper-protocol.md)/[ADR-0024](adr/0024-helper-execution.md)); neste modo o instalador precisa colocar uma cópia dele na raiz de dados.

### 2.2 Modo portátil
`portable.flag` presente → nenhuma escrita fora da pasta: sem PATH, sem registro, sem CA no store (usa `--insecure` ou instala CA manualmente), DNS via `hosts` só se o usuário pedir. Útil para pendrive, VM descartável, CI.

### 2.4 Case sensitivity dos diretórios
`www\`, `etc\`, `data\`, `logs\`, `tmp\` e `backups\` são criados com a flag NTFS `FILE_CS_FLAG_CASE_SENSITIVE_DIR` (herdada por subdiretórios) quando o modo paridade Linux está ligado (padrão). `bin\`, `app\`, `state\`, `certs\` e `manifests\` ficam no modo normal do Windows. Ver [15](15-linux-parity-case-sensitivity.md).

### 2.5 Fontes
Inter e JetBrains Mono são **embutidas no frontend** (woff2 de `@fontsource/inter` e `@fontsource/jetbrains-mono`, subconjuntos latin e latin-ext, importados em `apps/desktop/src/design/fonts.css`): ficam dentro do bundle da GUI e não existem como arquivos soltos em `apponts\`. São o último fallback das pilhas de `docs/design/tokens.json` (docs/07 §2.1); a GUI prefere as fontes do sistema.

## 3. O que toca fora da raiz (e como reverter)

| Item | Onde | Quando | Reversão |
|---|---|---|---|
| PATH do usuário | `HKCU\Environment\Path` + `WM_SETTINGCHANGE` (entradas no início do PATH do usuário, sem duplicatas, `REG_EXPAND_SZ` e demais entradas preservados; [ADR-0025](adr/0025-layout-staging-shims-user-path.md)) | Onboarding, se o usuário marcar (`[path]` do `forgebox.toml`: `bin\<tool>\current` e/ou `bin\shims`); nunca no modo portátil | Desinstalador remove entradas sob `<raiz>\bin\` (`Layout::remove_path_entries`) |
| Iniciar com Windows | `HKCU\...\Run\Forgebox` | Opt-in | Desinstalador remove |
| Protocolo `forgebox://` | `HKCU\Software\Classes\forgebox` | Instalação | Desinstalador remove |
| CA raiz | `CurrentUser\Root` (e opcional `LocalMachine\Root`) | Opt-in | Desinstalador oferece remover: a CA atual e toda raiz registrada em `certs\ca\trust.json` (thumbprint SHA-1 + escopo; inclui CAs antigas cuja remoção foi recusada numa rotação). Remover de `CurrentUser\Root` mostra o diálogo do Windows; `LocalMachine` vai pelo helper |
| Regra NRPT `.test` | Política DNS do sistema | Opt-in via helper | Desinstalador remove via helper |
| `hosts` | bloco gerenciado | Fallback | Bloco removido |
| Firewall | regras `Forgebox-*` | Só com "expor na LAN" | Removidas |
| Exclusão do Defender | pasta raiz | Opt-in (acelera muito PHP/Composer) | Removida |
| Uninstall info | `HKCU\...\Uninstall\Forgebox` | Instalação | — |

## 4. Desinstalação
Diálogo com três opções: **Manter tudo em `C:\Forgebox`** (só remove binários do app e entradas do sistema), **Remover tools mas manter dados** (`bin\`, `tmp\`, `state\`; mantém `www\ data\ etc\ certs\ backups\`), **Apagar tudo** (exige digitar "APAGAR"). Sempre reverte a tabela da seção 3.

## 5. Nome canônico de pasta de versão
`<version>[-<variant>...]` com variantes na ordem de `versioning.variants` do manifesto (`forge_manifest::version::canonical_name`): PHP (`ts`, `compiler`, `arch`) `8.3.37-nts-vs17-x64`; Apache (`compiler`, `arch`) `2.4.69-vs18-x64`; tools sem variantes, só `<version>`. O motor também reconhece nomes do Laragon (`php-8.3-Win32-vs16-x64`) via `discover` e os importa **sem renomear** (a pasta continua como está; o registro guarda o caminho).

## 6. Migração a partir do Laragon
`forge import laragon C:\laragon [--dry-run] [--copy-tools] [--move-data] [--convert-case]` (`system.import_laragon`, job; [ADR-0031](adr/0031-laragon-import.md)). A pasta do Laragon **só é lida** (a única execução são os comandos de versão dos manifestos, dentro de cada pasta de tool). Dois passos, os mesmos no `--dry-run` e na importação de verdade: `LaragonImporter::scan` monta um `ImportPlan` e `apply` registra o que ele contém; o resultado é um **relatório estruturado** (`ImportReportDto`: `kind`, `name`, `imported`, `note`; no `--dry-run`, `imported` = "seria importado") com cada item importado e cada item pulado **com o motivo**.

| Origem no Laragon | Vira | Como |
|---|---|---|
| `bin\<tool>\<pasta>` (PHP, Apache, Nginx, MySQL, MariaDB, Node, PostgreSQL...) | `tool_versions` (`source: import`) | Reconhecidas só pelos manifestos (`spec.discover`, M1-E1-T4); só entram versões que **o próprio executável respondeu** (versão tirada do nome da pasta fica de fora, com o motivo). Padrão: **referência** (a pasta não é copiada nem renomeada; o registro guarda o caminho). `--copy-tools`: copia para `bin\<tool>\<nome canônico>` (via `*.tmp`). A junction `bin\php\php` não é uma versão: ela define a **versão ativa** (`[php] Version` do ini é o reserva). Pastas sem manifesto (cmder, git, heidisql...) e o `composer` solto (sem versão identificável) são listadas como puladas |
| `etc\apache2\sites-enabled\auto.*.conf` e `etc\nginx\sites-enabled\*.conf` | sites explícitos | `Define`/`${VAR}` do Apache expandidos; `ServerName`/`ServerAlias`/`DocumentRoot` (Nginx: `server_name`/`root`/`listen ... ssl`). Arquivos que dividem a mesma pasta viram **um** site: o nome com o TLD do `HostnameFormat` é o host, os demais (`app.local`) viram aliases. Host em minúsculas e `_` vira `-` (`SIMPROS_MATRIZ.test` → `simpros-matriz.test`, anotado); aliases curinga ficam de fora. Driver detectado na pasta do projeto, **PHP = série da versão ativa do Laragon**, TLS se havia bloco `:443`. Outros `.conf` do Apache (`00-default`, `reverse.*`) e hosts de TLD não servido pelo Forgebox são pulados com o motivo |
| `data\<tool>-<versão>` (`mysql-8`, `postgresql-17`) | instância `<pasta>` do tool | Padrão: **referência** ao data dir do Laragon (`instances.data_dir` + marca `forgebox.external_data_dir` em `settings`; o daemon usa o caminho como está, em vez do `data.dir` do manifesto). A versão é a ativa do Laragon se servir à versão da pasta, senão a mais nova que servir. `--move-data`: `rename` para `data\<tool>\<nome>` no mesmo volume (outro volume, destino existente ou arquivo em uso: fica como referência, com o motivo); o servidor do Laragon precisa estar parado. Um data dir inicializado no Laragon mantém o `lower_case_table_names` com que foi criado (o servidor recusa trocá-lo), então não herda a paridade Linux de [15](15-linux-parity-case-sensitivity.md) §5 |
| `DocumentRootList` / `[apache] DocumentRoot` (`www\`) | pasta parked | Adicionada a `parked_dirs`; o que já virou site explícito não é duplicado. Pasta com conteúdo: o relatório **oferece** a conversão para case-sensitive; `--convert-case` roda a conversão de [15](15-linux-parity-case-sensitivity.md) §3.3 antes de registrar a pasta |
| `usr\laragon.ini` | preferências | `HostnameFormat` → TLD: **só relatado** (o DNS, os certificados e as regras de host nascem do TLD; trocar é `forge config set tld`). `DocumentRootList` → parked (acima). `DataDir` serve para achar a raiz original |

Idempotente: reexecutar relata "já registrada"/"já existe" e não cria nada. Instalação **movida ou copiada**: os `.conf` e o ini guardam o caminho antigo (`C:/laragon/www/app`); o que estiver abaixo da raiz gravada em `DataDir` é lido da pasta escaneada. As mensagens do relatório são pt-BR (chave `import.*` em `forge_sites::import::messages`).

## 7. Compatibilidade de atualização do Forgebox
- Updates do app nunca tocam `bin\ data\ www\ certs\ etc\`. Só `*.exe`, `app\` e migrations do `state\forgebox.db` (sempre forward-compatíveis; backup automático do `.db` antes, via `VACUUM INTO`, em `state\forgebox.db.bak-v<NNNN>-<UTC>`; um `.db` com schema mais novo que o binário é recusado, nunca rebaixado).
- Templates embutidos novos podem mudar o conteúdo dos **blocos gerenciados** em `etc\`; o update mostra diff antes de aplicar (ou aplica silenciosamente se o usuário escolher "sempre aceitar").

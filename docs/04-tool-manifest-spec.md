# 04 — Especificação do manifesto YAML (`forgebox.yaml`)

O manifesto é o contrato entre uma ferramenta e o motor. **Nenhuma tool existe para o Forgebox sem manifesto.** O mesmo arquivo serve para: catálogo oficial (`registry/tools/*.yaml`), registries de terceiros, manifesto embutido dentro do pacote da tool (`<zip>/forgebox.yaml`) e manifestos locais do usuário (`<root>/manifests/local/*.yaml`).

Há quatro `kind`s de manifesto, mais o índice do registry:

| kind | Para quê | Onde vive |
|---|---|---|
| `Tool` | Um software instalável (PHP, Nginx, MySQL, cloudflared, HeidiSQL...) | registry / embutido / local |
| `Extension` | Algo que se instala **dentro** de uma Tool (Xdebug, PostGIS, módulo Apache) | registry |
| `Stack` | Preset: conjunto de tools + instances + site template (Laravel, WordPress, Next+PHP) | registry |
| `Project` | Arquivo opcional na raiz de um projeto (`forgebox.yaml`) declarando o que ele precisa | repositório do usuário |
| `Registry` | `index.yaml` de um registry (§5) | raiz do registry |

Schema JSON gerado automaticamente (`cargo xtask schema`) em `registry/schema/{tool,extension,stack,project,registry}.v1.json`; IDEs têm autocomplete via `# yaml-language-server: $schema=...`.

Regras gerais (implementadas em `forge-manifest`):
- `apiVersion` é exatamente `forgebox.dev/v1`; `kind` decide o schema do resto do documento.
- **Chave desconhecida é erro** em qualquer nível (nada de typo ignorado em silêncio). Erros de YAML/schema trazem linha e coluna.
- Durações são texto com unidade (`"15s"`, `"2m"`, `"1h"`, `"1h 30m"`); número puro é recusado.
- Versões e séries são texto. O parser preserva o texto original mesmo sem aspas (`series: 8.10` continua `"8.10"`), mas prefira aspas: o JSON Schema do editor marca número onde se espera texto.
- Ações (`post_install`, `update.migrate`, `post_create`) são mapas de uma chave só: `- copy: {...}`, `- run: {...}`.
- `id`s de Tool/Extension/Stack seguem `[a-z0-9-]` (começam e terminam com letra/dígito, até 64 caracteres; `shims` e `current` são reservados).

---

## 1. `kind: Tool` — referência completa

```yaml
apiVersion: forgebox.dev/v1
kind: Tool

metadata:
  id: php                        # [a-z0-9-], único no registry
  name: PHP
  summary: Linguagem PHP (builds oficiais Windows)
  description: |
    Texto longo em Markdown exibido no catálogo.
  category: runtime              # runtime | webserver | database | cache | queue | search | storage | mail | tunnel | devtool | gui | library
  tags: [php, language, fastcgi]
  homepage: https://www.php.net
  docs: https://www.php.net/manual
  license: PHP-3.01
  icon: php.svg                  # relativo a registry/icons/ ou URL https (SVG, regra 7)
  maintainers: [{ name: Forgebox Team, url: https://github.com/forgebox }]
  deprecated: false              # true + replaced_by: <id> esconde do catálogo (mas mantém instalados)

spec:
  platforms: [windows-x64]       # windows-x64 | windows-arm64 | macos-arm64 | linux-x64 (futuro); não pode ser vazio
  runtime_deps: [vcredist-vs17]  # pré-requisitos do sistema; ids conhecidos pelo motor: vcredist-vc15|vs16|vs17|vs18 (VC++ x64 ≥ 14.10|14.20|14.30|14.50; docs/03 §1.5)
  depends_on: []                 # outras tools obrigatórias (ex. phpmyadmin → php)
  suggests: [composer, xdebug]   # UI oferece após instalar

  # ---------- Versionamento ----------
  versioning:
    scheme: semver               # semver | calver | string (§1.6)
    channels:
      stable: { constraint: ">=7.4 <9" }
      preview: { constraint: ">=8.6.0-0", prerelease: true }
    series_key: "{major}.{minor}" # agrupa versões na UI e define "série" para updates patch
    variants:                    # dimensões ortogonais de build; cada combinação é uma ToolVersion distinta
      - key: ts                  # values em ordem de preferência; `default` vem primeiro
        values: [nts, ts]
        default: nts
        label: { nts: Non Thread Safe, ts: Thread Safe }
      - key: compiler
        values: [vs17, vs16, vc15]
        derive: from_provider    # o provider informa; usuário não escolhe
      - key: arch                # a ordem das variantes é a do nome da pasta: 8.3.37-nts-vs17-x64 (docs/08 §5)
        values: [x64]
        default: x64

  # ---------- De onde vem ----------
  source:
    providers:                   # avaliados em ordem; o primeiro que resolve a versão vence (§1.5)
      - type: php-windows        # provider nativo: lê https://downloads.php.net/~windows/releases/releases.json (+ archives/)
        include_archives: true   # versões antigas em releases/archives/
        qa: false                # builds de QA/RC (channel preview)
      - type: http               # fallback genérico com padrão de URL
        url: "https://downloads.php.net/~windows/releases/archives/php-{version}-Win32-{compiler}-x64.zip"
        checksum: { type: sha256, url: "{url}.sha256" }   # ou inline por versão em `releases:`
    mirrors:
      - "https://mirror.exemplo.com/php/{file}"
    releases: []                 # lista estática opcional [{version, url, sha256, size, variants, published_at}] para tools sem API

  # ---------- Como instala ----------
  install:
    archive: zip                 # zip | 7z | tar.gz | tar.xz | file (arquivo único: .exe/.dll/.phar) | msi (não suportado em v1; extração `msiexec /a` ainda sem tarefa no roadmap, necessária para Memurai); omitido = deduzido do nome do arquivo
    strip_root: auto             # auto: remove a pasta de topo quando TODAS as entradas estão nela | true: exige essa pasta (erro se não houver) | false
    layout: versioned            # versioned → bin/<id>/<version>/ (padrão) | single → bin/<id>/ (tools sem versão)
    current_link: true           # cria/atualiza junction bin/<id>/current
    path:                        # subpastas a expor no PATH via `current` e shims
      - "."
    shims: [php, php-cgi, phpdbg, pear, pecl]    # bin/shims/<nome>.cmd → bin/<id>/current/<nome>.exe
    verify:
      command: "php.exe -v"
      regex: "^PHP (\\d+\\.\\d+\\.\\d+)"          # grupo 1 = versão instalada (opcional)
    post_install:                # ações declarativas, executadas em ordem, idempotentes
      - copy: { from: "php.ini-development", to: "{{ paths.etc }}/php/{{ series }}/php.ini", if_missing: true }
      - render: { template: "php/php.ini.j2", to: "{{ paths.etc }}/php/{{ series }}/conf.d/10-forgebox.ini" }
      - mkdir: "{{ paths.tmp }}/php/{{ series }}/sessions"
      - mkdir: "{{ paths.tmp }}/php/{{ series }}/uploads"   # upload_tmp_dir do overlay
      # - run: { command: "bin/tool.exe --init", cwd: "{{ paths.version }}", timeout: 60s }   # só executáveis da própria versão (regra 6)
    uninstall:
      keep: ["{{ paths.etc }}/php/{{ series }}"]  # o que preservar ao remover a versão
    size_estimate_mb: 35

  # ---------- Configuração exposta na UI ----------
  config:
    files:
      - id: php_ini              # [a-z0-9_]; referenciado por quick_settings[].file, ui.actions[].open e {{ instance.config.<id> }}
        path: "{{ paths.etc }}/php/{{ series }}/php.ini"
        format: ini              # ini | conf-directive | yaml | json | args | text
        label: php.ini
        editable: true
    quick_settings:              # viram controles nativos na UI (§1.3)
      - key: memory_limit
        label: Memory limit
        file: php_ini
        ini: "memory_limit"
        type: select
        options: [128M, 256M, 512M, 1G, -1]
        default: 512M
        apply: reload            # reload (recycle do pool) | restart (padrão) | none
      - key: upload_max_filesize
        file: php_ini
        ini: "upload_max_filesize"
        type: text
        default: 128M
        also_set: { post_max_size: "{{ value }}" }
      - key: max_execution_time
        file: php_ini
        ini: "max_execution_time"
        type: number
        default: 120
      - key: display_errors
        file: php_ini
        ini: "display_errors"
        type: toggle
        on: "On"
        off: "Off"
        default: on
      - key: opcache
        label: OPcache
        file: php_ini
        ini: "opcache.enable"
        type: toggle
        on: "1"
        off: "0"
    extensions:                  # lista de extensões bundled; UI mostra como toggles
      source: bundled            # bundled → ext/*.dll do próprio zip
      ini_dir: "{{ paths.etc }}/php/{{ series }}/conf.d"
      pattern: "php_{name}.dll"  # precisa de {name}
      defaults_on: [curl, mbstring, openssl, pdo_mysql, pdo_sqlite, pdo_pgsql, fileinfo, gd, intl, zip, sodium, exif, bcmath]
      zend_extensions: [{ name: opcache, constraint: "<8.5" }]  # PHP 8.5+ embute o OPcache
    # zend_extensions: itens `nome` ou `{ name, constraint }`; `constraint` usa a mesma sintaxe de
    # versioning.channels.*.constraint (scheme da tool) e o item só é emitido quando casa com a versão
    # instalada (ausente = sempre). Resolvido por ExtensionsConfig::zend_extensions_for.
    env:                         # variáveis injetadas nos processos desta tool
      PHPRC: "{{ paths.etc }}/php/{{ series }}"
      PHP_INI_SCAN_DIR: "{{ paths.etc }}/php/{{ series }}/conf.d"

  # ---------- Serviço (opcional) ----------
  service:
    kind: pool                   # none | daemon | pool | oneshot | gui; cada kind com processo tem seu bloco (daemon:, pool:, oneshot:, gui:)
    # kind: pool → gerenciado pelo forge-fcgi (só faz sentido para PHP)
    pool:
      exe: php-cgi.exe
      args: ["-b", "127.0.0.1:{{ port }}", "-c", "{{ paths.etc }}/php/{{ series }}/php.ini"]
      protocol: fastcgi
      port_range: php-pools      # nome de faixa definida em forgebox.toml (regra 3)
      scaling: { min: 2, max: 12, idle_timeout: 120s }
      recycle: { max_requests: 500, max_rss_mb: 256, max_age: 1h }
      env: { PHP_FCGI_MAX_REQUESTS: "{{ recycle.max_requests }}" }
      health: { fastcgi: "/_forgebox/health.php" }

  # ---------- Descoberta de instalação manual ----------
  discover:
    match_files: [php.exe, php-cgi.exe]
    version_command: "php.exe -r \"echo PHP_VERSION;\""
    # version_regex: "PHP (\\d+\\.\\d+\\.\\d+)"   # grupo 1 = versão; sem regex, a saída inteira (trim)
    variant_detect:
      ts: { command: "php.exe -i", regex: "Thread Safety => (enabled|disabled)", map: { enabled: ts, disabled: nts } }

  # ---------- Atualização ----------
  update:
    policy: auto-patch           # manual | notify (padrão) | auto-patch | auto-minor | auto
    retention: 1                 # versões anteriores mantidas por série para rollback
    check_interval: 6h
    migrate:                     # o que levar da versão antiga para a nova na mesma série
      - copy_dir: { from: "ext/", to: "ext/", only_missing: true }   # extensões adicionadas pelo usuário (Xdebug etc.)

  # ---------- UI ----------
  ui:
    color: "#777BB4"
    actions:                     # botões no card da tool/instance; cada um com exatamente um de open | shell | browse
      - { label: "php.ini", icon: file-text, open: config:php_ini }
      - { label: "Terminal com esta versão", icon: terminal, shell: { path_prepend: ["{{ paths.version }}"] } }
      - { label: "Info (phpinfo)", icon: info, browse: "http://localhost/_forgebox/phpinfo?v={{ version }}" }
    badges: ["{{ variant.ts }}", "{{ variant.compiler }}"]
```

### 1.1 `service.kind`
| kind | Significado | Bloco | Exemplos |
|---|---|---|---|
| `none` | Não é processo (biblioteca, CLI) | — | composer, git, php (o CLI), python |
| `daemon` | Processo de longa duração gerenciado pelo supervisor | `service.daemon` (§1.2) | nginx, httpd, mysqld, postgres, mailpit, memurai, minio, meilisearch |
| `pool` | Pool FastCGI gerenciado por `forge-fcgi` | `service.pool` | php |
| `oneshot` | Executa e termina; usado em `post_install`/ações | `service.oneshot` `{exe, args, cwd, env, timeout}` | `mysqld --initialize`, `pg_ctl initdb` |
| `gui` | App gráfico lançado sob demanda, fora do Job | `service.gui` `{exe, args, cwd, env}` | heidisql, dbeaver, notepad++ |

O bloco do `kind` é obrigatório e blocos de outro `kind` são erro. `exe` é sempre relativo à pasta da versão.

### 1.2 `service.daemon` (referência)
```yaml
service:
  kind: daemon
  daemon:
    multi_instance: true           # permite mysql "default" e "legacy" com data dirs/portas próprias
    exe: bin/mysqld.exe
    args: ["--defaults-file={{ instance.config.my_ini }}", "--console"]
    cwd: "{{ paths.version }}"
    env: {}
    console_group: true            # CREATE_NEW_PROCESS_GROUP (para CTRL_BREAK); padrão true
    ports:
      - { name: main, default: 3306, env: null, range: databases }   # range deve existir (regra 3)
    check: { command: "bin/mysqld.exe --defaults-file={{ instance.config.my_ini }} --validate-config" }   # pré-checagem antes do start e do restart (docs/03 §1.5); saída ≠ 0 = config_invalid; prazo fixo de 30 s
    start_timeout: 45s
    health:
      tcp: "127.0.0.1:{{ instance.ports.main }}"   # ou http: {url, expect_status} ou command: {cmd, exit_code}; exatamente um
      interval: 5s
      success_threshold: 1
      failure_threshold: 3
    stop:
      command: "bin/mysqladmin.exe --port={{ instance.ports.main }} -uroot shutdown"
      timeout: 20s
      then: ctrl-break               # ctrl-c | ctrl-break | close | kill: próximo passo se o comando não parar
    reload: null                     # ou { command: "..." } ou { signal: ctrl-c | ctrl-break }
    restart_policy: { on: failure, max: 3, window: 60s }   # on: never | failure | always; max/window limitam `failure` (`always` não tem limite); espera exponencial 1s..30s entre reinícios (docs/03 §1.4)
    logs:
      - { label: Error log, path: "{{ paths.logs }}/mysql/{{ instance.name }}/error.log", format: mysql }
    data:
      dir: "{{ paths.data }}/mysql/{{ instance.name }}"
      init:                          # inicialização de data dir na 1ª vez
        command: "bin/mysqld.exe --defaults-file={{ instance.config.my_ini }} --initialize-insecure"
    depends_on: []
    autostart: true
```

### 1.3 `config.quick_settings[]`
`type`: `toggle`, `select`, `number`, `text`, `path`, `port`, `slider{min,max,step}`, `multi` (lista de checkboxes). O `format` do arquivo (`config.files[].format`: `ini | conf-directive | yaml | json | args | text`) decide como o valor é gravado; o alvo dentro do arquivo é **uma** destas chaves: `ini` (chave do ini), `directive` (conf-directive), `pointer` (caminho com pontos em yaml/json) ou `arg` (flag em `args`). Para `conf-directive` (Apache/Nginx), o motor edita só dentro do bloco gerenciado do arquivo. `text` é editável, mas sem quick settings.

| Campo | Vale para | Regra |
|---|---|---|
| `options` | `select`, `multi` | obrigatório nesses tipos; proibido nos demais |
| `on` / `off` | `toggle` | valor gravado; ausentes = booleano nativo do formato |
| `min` / `max` / `step` | `number`, `slider` | `slider` exige `min` e `max`; `min <= max`; `step > 0` |
| `reserve` | `port` | reserva a porta no alocador (Xdebug 9003) |
| `default` | todos | `toggle`: true/false/on/off; `select`: um de `options`; `multi`: lista contida em `options`; `port`: 1–65535; `number`/`slider`: dentro de min/max |
| `apply` | todos | `reload` \| `restart` (padrão) \| `none` |
| `also_set` | todos | outras chaves gravadas com o mesmo valor (`{{ value }}`) |

### 1.4 Contexto de template nos manifestos
Strings dos manifestos só **substituem variáveis**: `{{ caminho.da.variavel }}`, com filtros opcionais (`{{ paths.etc | posix_path }}`). Literais, operadores e blocos `{% %}` são erro (regra 5). Variáveis permitidas:

| Onde | Variáveis |
|---|---|
| Tool e Extension (qualquer campo de `spec`) | `{{ version }}`, `{{ series }}`, `{{ variant.<key> }}` (key declarada em `versioning.variants`; em Extension, as da tool alvo), `{{ paths.root\|bin\|etc\|data\|logs\|tmp\|www\|certs\|version }}`, `{{ instance.name\|data_dir }}`, `{{ instance.ports.<n> }}` (porta declarada em `service.daemon.ports`), `{{ instance.config.<id> }}` (id de `config.files`), `{{ prefs.<chave> }}` |
| `config.quick_settings[]` | + `{{ value }}` (valor escolhido, em `also_set`) |
| `service.pool` | + `{{ port }}`, `{{ recycle.max_requests\|max_rss_mb\|max_age }}`, `{{ scaling.min\|max\|idle_timeout }}` |
| Stack e Project | `{{ dir }}`, `{{ site.slug\|host\|root }}`, `{{ paths.* }}`, `{{ prefs.* }}`, `{{ instances.<tool>.<instance>.ports.<n>\|config.<id>\|name\|data_dir }}` (tool listada em `spec.tools`/`spec.services`) |

Qualquer outra chave é erro de lint. Placeholders de URL dos providers usam chaves simples (`{version}`, `{file}`) e não são templates (§1.5).

### 1.5 Providers (`source.providers[].type`)
Campos de cada provider (comportamento em [05](05-providers-and-updates.md) §1):

| type | Campos | Checksum upstream |
|---|---|---|
| `php-windows` | `include_archives` (padrão `true`), `qa` (padrão `false`) | sim |
| `apache-lounge` | `builds` (páginas, padrão `[vs17, vs18]`), `asset` (regex do arquivo; padrão: o zip do httpd) | sim |
| `nginx-org` | — | **não** (só PGP) |
| `github-release` | `repo` (`owner/name`), `asset` (regex), `checksum: {type, asset}` (`checksums.txt`, `{asset}.sha256`; no nome do asset, `{asset}` e `{version}` são substituídos), `prerelease` (padrão `false`), `version_regex` (padrão: tag sem `v`) | se `checksum` |
| `mysql`, `postgresql-edb` | — (lista curada em `releases:`) | **não** |
| `mariadb`, `nodejs-dist` | — | sim |
| `python-embed` | `package: embed \| nuget` (padrão `embed`) | **não** (curado) |
| `http` | `url` (placeholders `{version}`, `{series}`, `{major}`, `{minor}`, `{patch}`, chaves de variante, `vars`), `checksum: {type: sha256\|sha512, url}`, `vars: {nome: {valor_da_variante: texto}}`, `version_source: {url, regex \| json_path}`; sem `releases:` precisa de `version_source` | se `checksum` |
| `local` | `path` (opcional) | calculado na hora |

Placeholders de URL usam chaves simples: `{version}`, `{series}`, `{major}`, `{minor}`, `{patch}`, as chaves de variante (`{compiler}`), os nomes de `vars`, `{file}` (nome do arquivo, em `mirrors` e `checksum.url`) e `{url}` (em `checksum.url`); em Extension, também `{<extends>_series}` (série da versão alvo, ex. `{php_series}`) e as variantes da versão alvo. URLs de download (`url`, `mirrors`, `releases[].url`, `checksum.url`, `version_source.url`) são `https://`; `mirrors` precisam de `{file}`. Provider sem checksum upstream exige `sha256` em **todas** as `source.releases[]` (regra 2; o bot do registry cura esses hashes, [05](05-providers-and-updates.md) §6).

### 1.6 Versões, restrições e variantes
- **`scheme: semver`**: `8` e `8.4` são lidos como `8.0.0`/`8.4.0`; pré-releases (`8.6.0-RC1`) vêm antes da release. Mais de 3 componentes numéricos exige `calver`/`string`.
- **`calver` / `string`**: segmentos separados por `.` `-` `_` `+`, comparados um a um: ambos numéricos por valor (`2024.9 < 2024.10`), ambos texto de forma lexicográfica, número < texto, prefixo antes (`1.2 < 1.2.1`). `calver` exige o primeiro segmento numérico.
- Versões só usam `[0-9A-Za-z._+-]` (viram nome de pasta).
- **Restrições**: comparadores separados por espaço ou vírgula são AND; `||` separa alternativas. Em `semver`: `= > >= < <= ~ ^ *` e `3.4.*`; **versão sem operador é exata** e parcial casa a série (`8.4` = `8.4.*`, diferente do Cargo). Em `calver`/`string`: `= > >= < <=`; sem operador ou `X.*` casa o prefixo.
- **Canais**: pré-releases só entram em canal com `prerelease: true` (aí comparadas por precedência simples: `>=8.6.0-0` casa `8.6.1-RC1`).
- **`series_key`**: placeholders `{major}`, `{minor}`, `{patch}` (componente ausente = `0`).
- **Variantes**: `key` em `[a-z0-9_]`; `values` em `[a-z0-9._-]` (nome de pasta), em **ordem de preferência**; `default` ganha de todos. Builds são ordenados por versão (mais nova primeiro) e depois por variante, na ordem de `variants` (`nts` antes de `ts`; `vs17` antes de `vs16`). O nome canônico da pasta é `<version>-<valor>...` na ordem de `variants` ([08](08-release-layout.md) §5).

### 1.7 Padrões
| Campo | Padrão |
|---|---|
| `versioning` | `scheme: semver`, `series_key: "{major}.{minor}"`, sem canais e sem variantes |
| `install` | `archive` deduzido do arquivo, `strip_root: auto`, `layout: versioned`, `current_link: true`, `path: []` |
| `config.files[].editable` | `true` |
| `quick_settings[].apply` / `toggle.apply` | `restart` |
| `service.kind` | `none` |
| `service.daemon` | `console_group: true`, `start_timeout: 30s`, `stop: {timeout: 15s, then: ctrl-break}`, `restart_policy: {on: failure, max: 3, window: 60s}`, `health.interval: 5s`, thresholds 1/3, `http.expect_status: 200`, `autostart: false`, `multi_instance: false` |
| `service.pool` | `protocol: fastcgi`, `port_range: php-pools`, `scaling: {min: 2, max: 12, idle_timeout: 120s}`, `recycle: {max_requests: 500, max_rss_mb: 256, max_age: 1h}` |
| `update` | `policy: notify`, `retention: 1`, `check_interval: 6h` |
| `processes.*` (Project) | `restart: failure`, `autostart: true`, `proxy: false` |

---

## 2. `kind: Extension`

```yaml
apiVersion: forgebox.dev/v1
kind: Extension
metadata: { id: xdebug, name: Xdebug, category: devtool, icon: xdebug.svg }
spec:
  extends: php
  compat:                        # resolvido contra a ToolVersion alvo; a primeira entrada que casa vence
    - { tool: "~8.3", variant: { ts: nts, compiler: vs16 }, version: "3.4.*" }   # tool: restrição sobre a versão da tool estendida
    - { tool: "~8.4", variant: { ts: nts, compiler: vs17 }, version: "3.4.*" }
  source:
    providers:
      - type: http
        url: "https://xdebug.org/files/php_xdebug-{version}-{php_series}-{compiler}-{ts_flag}-x86_64.dll"
        vars: { ts_flag: { nts: "nts", ts: "" } }
        checksum: { type: sha256, url: "https://xdebug.org/files/{file}.sha256" }   # ilustrativo
    releases:
      - { version: "3.4.1" }
  install:
    target: "{{ paths.version }}/ext/php_xdebug.dll"   # dentro da versão alvo: o arquivo (arquivo único) ou a pasta onde o arquivo compactado é extraído
    post_install:
      - render: { template: "php/xdebug.ini.j2", to: "{{ paths.etc }}/php/{{ series }}/conf.d/20-xdebug.ini" }
  config:
    quick_settings:
      - { key: mode, ini: "xdebug.mode", type: multi, options: [off, debug, develop, coverage, profile, trace], default: [debug, develop] }
      - { key: start_with_request, ini: "xdebug.start_with_request", type: select, options: [yes, trigger, no], default: trigger }
      - { key: client_port, ini: "xdebug.client_port", type: port, default: 9003, reserve: true }
  toggle: { enable: "zend_extension=php_xdebug.dll", disable: ";zend_extension=php_xdebug.dll", apply: reload }
```
`source.releases[].variants` de uma Extension aceita chaves livres (as variantes reais são as da tool alvo): o catálogo usa `php_series` (`{php_series}`) para distinguir DLLs que compartilham `ts`/`compiler` (PHP 8.4 e 8.5 são ambos `nts`/`vs17`). Como xdebug.org não publica `.sha256`, cada release traz `url` e `sha256` curados.

`install` de Extension aceita `archive`, `strip_root`, `target` (obrigatório, começa com `{{ paths.version }}`), `post_install`, `uninstall` e `size_estimate_mb`. Versões de Extension usam `semver`.

---

## 3. `kind: Stack`

```yaml
apiVersion: forgebox.dev/v1
kind: Stack
metadata: { id: laravel, name: Laravel, summary: "PHP 8.4 + Nginx + MySQL 8 + Redis + Mailpit", icon: laravel.svg }
spec:
  tools:
    - { id: php, series: "8.4", extensions: [xdebug] }
    - { id: composer }
    - { id: nginx }
    - { id: mysql, series: "8.4", instance: default }
    - { id: memurai }
    - { id: mailpit }
    - { id: nodejs, series: "22" }
  site:
    create_with: "composer create-project laravel/laravel {{ dir }}"
    driver: laravel
    php: "8.4"
    secure: true
    env_patch:                   # merge em .env após criar
      DB_HOST: 127.0.0.1
      DB_PORT: "{{ instances.mysql.default.ports.main }}"
      DB_DATABASE: "{{ site.slug }}"
      REDIS_PORT: "{{ instances.memurai.default.ports.main }}"
      MAIL_HOST: 127.0.0.1
      MAIL_PORT: "{{ instances.mailpit.default.ports.smtp }}"
    post_create:
      - create_database: { instance: mysql/default, name: "{{ site.slug }}" }   # <tool>/<instance>, tool de spec.tools
      - run: { command: "php artisan key:generate", cwd: "{{ dir }}" }
```

---

## 4. `kind: Project` (arquivo `forgebox.yaml` na raiz do projeto)

Equivalente ao `herd.yml`/`.ddev/config.yaml`, porém mais completo e opcional. Quando presente, `forge up` prepara tudo.

```yaml
apiVersion: forgebox.dev/v1
kind: Project
metadata: { name: loja }
spec:
  site:
    host: loja.test              # minúsculo; padrão: nome da pasta + TLD
    aliases: [admin.loja.test]
    root: public
    driver: laravel
    server: nginx                # apache | nginx | caddy | frankenphp
    php: "8.3"                   # série ou versão exata "8.3.36"
    php_ini: { memory_limit: 1G, upload_max_filesize: 256M }
    xdebug: trigger              # inherit | off | trigger | always
    secure: true
  services:
    - { tool: mysql, series: "8.4", instance: default, database: loja }
    - { tool: memurai }
    - { tool: mailpit }
  runtimes:
    - { tool: nodejs, version: "22" }       # usado por `forge shell` e pelos processes abaixo
  processes:                     # gerenciados pelo supervisor quando o site está "up" (docs/06 §4.2)
    vite:   { cmd: "npm run dev -- --port $PORT", port: 5173, proxy_path: null }
    queue:  { cmd: "php artisan queue:work", restart: always }
    sched:  { cmd: "php artisan schedule:work" }
    # web:  { cmd: "npm start -- --port $PORT", port: auto, proxy: true, runtime: nodejs@22, autostart: on-visit, cwd: ., env: {} }
  proxies:                       # sites extras do tipo reverse proxy
    - { host: app.loja.test, upstream: "http://127.0.0.1:3000", websocket: true, secure: true }   # ex. Next.js
  share: { provider: cloudflared }
```

---

## 5. Registry (`index.yaml`)

```yaml
apiVersion: forgebox.dev/v1
kind: Registry
metadata: { name: official, url: https://github.com/forgebox/registry }
spec:
  schema_version: 1
  updated_at: 2026-10-26T00:00:00Z
  tools:                         # tools e extensions
    - { id: php, path: tools/php.yaml, sha256: "<64 hex>", categories: [runtime], latest: "8.4.14" }
    - { id: nginx, path: tools/nginx.yaml, sha256: "<64 hex>" }
  stacks:
    - { id: laravel, path: stacks/laravel.yaml, sha256: "<64 hex>" }
```
- `schema_version` é `1`; `sha256` tem 64 dígitos hexadecimais **e é conferido pelo lint contra os bytes do arquivo** (`index_mismatch`); `path` é relativo à raiz do registry.
- `registry/index.yaml` é **gerado** por `cargo xtask registry-index` (ordenado por id; `categories` vem de `metadata.category`; `latest` é a maior versão estável de `source.releases` quando houver, senão ausente; `updated_at` só muda quando as entradas mudam). Depois de editar qualquer manifesto, rode o comando; `lint-registry` falha com `index_mismatch` se o índice estiver velho.
- Assinatura ([ADR-0018](adr/0018-detached-index-signature.md)): o índice é assinado por uma **assinatura minisign destacada, `index.yaml.minisig`, ao lado dele, sobre os bytes crus de `index.yaml`**. Não há canonicalização nem dependência do emissor YAML: o payload é exatamente o arquivo, então qualquer implementação de minisign assina e verifica, inclusive autores de registries de terceiros e o bot do registry (`minisign -S -s <chave> -m index.yaml`). `spec.signature` **não existe mais** no schema (um `signature:` antigo é recusado por `deny_unknown_fields`). `cargo xtask registry-index` gera o índice sem assinatura e `cargo xtask registry-sign --key <secreta> [--dir <registry>]` escreve `index.yaml.minisig` (e confere com o próprio loader antes de gravar); editar o `index.yaml` depois de assinar, por um byte que seja, invalida a assinatura e exige `registry-sign` de novo. Como os bytes são o contrato, toda fonte entrega o arquivo intacto: `Http` baixa `<URL do index>.minisig`, `Git` faz checkout byte-exato (`core.autocrlf=false`; `.gitattributes` fixa LF), `Dir` lê o par de arquivos, e o cache guarda os dois. O loader só aceita um índice sem `.minisig` se o registry estiver marcado `trusted_unsigned` (desenvolvimento; aparece como "não verificado"); uma assinatura presente e errada (outra chave, índice editado, arquivo ilegível) é **sempre** recusada, e uma assinatura sem chave configurada só carrega com `trusted_unsigned`. O `key_id` mostrado vem da chave pública configurada que verificou. A chave de desenvolvimento pública é `registry/keys/dev.pub` (embutida em `forge_registry::index::DEV_PUBLIC_KEY`); a secreta não é commitada. `lint-registry` confere a assinatura quando `registry/keys/dev.pub` existe. Um teste golden (`forge-registry`, `index.rs`) fixa o hash do `index.yaml` da fixture e verifica a assinatura commitada, para pegar mudanças de comportamento em atualizações de dependências.
- Verificação no loader (`forge-registry`): (1) assinatura do índice **antes** de qualquer manifesto ser lido/baixado; (2) sha256 de cada arquivo contra o índice (diferença = a carga inteira falha e o cache anterior continua valendo); (3) cada manifesto é parseado e validado; um manifesto inválido, com `id`/`kind` diferente do índice ou, em registry não oficial, com `post_install.run` é **ignorado e listado** (`skipped`), sem derrubar o resto. `path` do índice é confinado a `tools/` ou `stacks/`, termina em `.yaml`, sem `..`, `:`, `\` nem caminho absoluto.
- Fontes: pasta local (`Dir`, também `file://` e compartilhamentos de rede), `Http` (URL do `index.yaml`; manifestos relativos a ela; `ETag`/`If-None-Match`) e `Git` (URL do repositório com `index.yaml` na raiz; usa o comando `git` da máquina, `--depth 1`, sem libgit2 e sem caminho especial para o GitHub; sem `git` no PATH o erro é claro e dá para usar `Http` com `raw.githubusercontent.com`). Esquemas aceitos para git: `https`, `http`, `ssh`, `git`, `file`.
- Cache: `state/registry/<nome>/` guarda só o que foi verificado (`index.yaml` + manifestos listados) e é **reverificado a cada leitura**; o cache guarda também o `index.yaml.minisig`; metadados (etag/commit, `last_refresh_at`, `verification`, `index_sha256`, `last_error`, `ttl_secs`, `trusted_unsigned`, `public_key`) ficam na tabela `registries` (migration `0002_registry_loader.sql`). Dentro do TTL (padrão 1 h) `load` não toca a fonte; `refresh` sempre pergunta (condicional) e só reescreve o cache se algo mudou. As operações de **um** registry são serializadas (`load`, jobs de `refresh`, `remove`; ver o módulo `service` e [03](03-engine.md) §7.2): nunca há dois escritores do mesmo `state/registry/<nome>/`. `add` não espera, mas inicia uma nova *geração*: o que estava em andamento sob a configuração antiga é descartado e refeito (um registry rebaixado de oficial para terceiro nunca serve manifestos com `post_install.run`), e `remove` cancela o refresh em curso e só apaga o cache depois de ter a vez. Trocar a URL de um registry `Git` busca sempre da URL configurada (`git fetch -- <url> HEAD`), nunca do remote gravado no clone original.
- Registries de terceiros: `forge registry add https://.../index.yaml` (exibidos com badge "terceiro"; manifestos com `post_install.run` são recusados, [11](11-security.md) §2). A flag `official` por registry (coluna `registries.official`) é o que libera `run`.
- Manifesto **embutido no pacote**: se o zip da tool contém `forgebox.yaml` na raiz, `forge tool add-local <zip>` instala direto. Permite que qualquer autor distribua sua ferramenta "pronta para Forgebox" sem passar pelo registry.

---

## 6. Regras de lint (`cargo xtask lint-registry`)
Implementadas em `forge_manifest::lint` (registry inteiro) e `forge_manifest::parse::validate` (um manifesto). Cada problema tem um código estável (`IssueCode`, também chave de i18n) e a regra a que pertence.

1. `id` único (Tool + Extension em `tools/`, Stack em `stacks/`), arquivo `<id>.yaml` na pasta do seu `kind`; `apiVersion`/`kind` válidos e schema respeitado (chave desconhecida, tipo errado); `platforms` não vazio; referências a outros manifestos existem (`depends_on`, `suggests`, `replaced_by`, `extends` → Tool, ferramentas e extensions de Stack); `index.yaml` lista exatamente os manifestos existentes, com `id`, `kind` e `sha256` corretos. Demais validações semânticas (variantes, restrições, referências internas, URLs, hosts) também caem aqui.
2. Provider sem checksum upstream (`http`/`github-release` sem `checksum`, `nginx-org`, `mysql`, `postgresql-edb`, `python-embed`) precisa de `releases[].sha256` em todas as releases.
3. `service.daemon.ports[].range` e `service.pool.port_range` devem existir nas faixas conhecidas (`forge_core::config`, [03](03-engine.md) §4).
4. Templates referenciados (`post_install[].render.template`) devem existir em `forge-render/templates/` ou no próprio registry (`<registry>/templates/<caminho>`).
5. Variáveis de template desconhecidas (§1.4) ou expressões que não são variáveis = erro.
6. `post_install.run` só pode invocar executáveis dentro de `{{ paths.version }}` (caminho relativo, resolvido lá e nunca via PATH; sem `..`, caminho absoluto ou variáveis); destinos de `copy`/`render`/`mkdir` começam com `{{ paths.<dir> }}` e não saem dele; `Extension.install.target` começa com `{{ paths.version }}`; registries de terceiros não podem declarar `run`.
7. Ícones em SVG ≤ 20 KB (arquivo em `registry/icons/` ou URL `https://` para `.svg`), sem `<script>`; tudo em `registry/icons/` é `.svg`.

# 04 — Especificação do manifesto YAML (`forgebox.yaml`)

O manifesto é o contrato entre uma ferramenta e o motor. **Nenhuma tool existe para o Forgebox sem manifesto.** O mesmo arquivo serve para: catálogo oficial (`registry/tools/*.yaml`), registries de terceiros, manifesto embutido dentro do pacote da tool (`<zip>/forgebox.yaml`) e manifestos locais do usuário (`<root>/manifests/local/*.yaml`).

Há quatro `kind`s:

| kind | Para quê | Onde vive |
|---|---|---|
| `Tool` | Um software instalável (PHP, Nginx, MySQL, cloudflared, HeidiSQL...) | registry / embutido / local |
| `Extension` | Algo que se instala **dentro** de uma Tool (Xdebug, PostGIS, módulo Apache) | registry |
| `Stack` | Preset: conjunto de tools + instances + site template (Laravel, WordPress, Next+PHP) | registry |
| `Project` | Arquivo opcional na raiz de um projeto (`forgebox.yaml`) declarando o que ele precisa | repositório do usuário |

Schema JSON gerado automaticamente (`cargo xtask schema`) em `registry/schema/*.v1.json`; IDEs têm autocomplete via `# yaml-language-server: $schema=...`.

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
  icon: php.svg                  # relativo a registry/icons/ ou URL https
  maintainers: [{ name: Forgebox Team, url: https://github.com/forgebox }]
  deprecated: false              # true + replaced_by: <id> esconde do catálogo (mas mantém instalados)

spec:
  platforms: [windows-x64]       # windows-x64 | windows-arm64 | macos-arm64 | linux-x64 (futuro)
  runtime_deps: [vcredist-vs17]  # pré-requisitos do sistema; ids conhecidos pelo motor
  depends_on: []                 # outras tools obrigatórias (ex. phpmyadmin → php)
  suggests: [composer, xdebug]   # UI oferece após instalar

  # ---------- Versionamento ----------
  versioning:
    scheme: semver               # semver | calver | string
    channels:
      stable: { constraint: ">=7.4 <9" }
      preview: { constraint: ">=8.6.0-0", prerelease: true }
    series_key: "{major}.{minor}" # agrupa versões na UI e define "série" para updates patch
    variants:                    # dimensões ortogonais de build; cada combinação é uma ToolVersion distinta
      - key: ts
        values: [nts, ts]
        default: nts
        label: { nts: Non Thread Safe, ts: Thread Safe }
      - key: compiler
        values: [vc15, vs16, vs17]
        derive: from_provider    # o provider informa; usuário não escolhe

  # ---------- De onde vem ----------
  source:
    providers:                   # avaliados em ordem; o primeiro que resolve a versão vence
      - type: php-windows        # provider nativo: lê https://downloads.php.net/~windows/releases/releases.json (+ archives/)
        include_archives: true   # versões antigas em releases/archives/
        qa: false                # builds de QA/RC (channel preview)
      - type: http               # fallback genérico com padrão de URL
        url: "https://downloads.php.net/~windows/releases/archives/php-{version}-Win32-{compiler}-x64.zip"
        checksum: { type: sha256, url: "{url}.sha256" }   # ou inline por versão em `releases:`
    mirrors:
      - "https://mirror.exemplo.com/php/{file}"
    releases: []                 # lista estática opcional [{version, url, sha256, size, variants...}] para tools sem API

  # ---------- Como instala ----------
  install:
    archive: zip                 # zip | 7z | tar.gz | tar.xz | exe-portable | msi (não suportado em v1)
    strip_root: auto             # true | false | auto (remove diretório único de topo)
    layout: versioned            # versioned → bin/<id>/<version>/ (padrão) | single → bin/<id>/ (tools sem versão)
    current_link: true           # cria/atualiza junction bin/<id>/current
    path:                        # subpastas a expor no PATH via `current` e shims
      - "."
    shims: [php, php-cgi, phpdbg, pear, pecl]    # bin/shims/<nome>.cmd → bin/<id>/current/<nome>.exe
    verify:
      command: "php.exe -v"
      regex: "^PHP (\\d+\\.\\d+\\.\\d+)"
    post_install:                # ações declarativas, executadas em ordem, idempotentes
      - copy: { from: "php.ini-development", to: "{{ paths.etc }}/php/{{ series }}/php.ini", if_missing: true }
      - render: { template: "php/php.ini.j2", to: "{{ paths.etc }}/php/{{ series }}/conf.d/10-forgebox.ini" }
      - mkdir: "{{ paths.tmp }}/php/{{ series }}/sessions"
      - mkdir: "{{ paths.tmp }}/php/{{ series }}/uploads"   # upload_tmp_dir do overlay
    uninstall:
      keep: ["{{ paths.etc }}/php/{{ series }}"]  # o que preservar ao remover a versão
    size_estimate_mb: 35

  # ---------- Configuração exposta na UI ----------
  config:
    files:
      - id: php_ini
        path: "{{ paths.etc }}/php/{{ series }}/php.ini"
        format: ini
        label: php.ini
        editable: true
    quick_settings:              # viram controles nativos na UI (toggle, slider, select, text)
      - key: memory_limit
        label: Memory limit
        file: php_ini
        ini: "memory_limit"
        type: select
        options: [128M, 256M, 512M, 1G, -1]
        default: 512M
        apply: reload            # reload (recycle do pool) | restart | none
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
      pattern: "php_{name}.dll"
      defaults_on: [curl, mbstring, openssl, pdo_mysql, pdo_sqlite, pdo_pgsql, fileinfo, gd, intl, zip, sodium, exif, bcmath]
      zend_extensions: [opcache]  # linhas zend_extension do overlay (PhpModel.zend_extensions); vazio no PHP 8.5+, que embute o OPcache
    env:                         # variáveis injetadas nos processos desta tool
      PHPRC: "{{ paths.etc }}/php/{{ series }}"
      PHP_INI_SCAN_DIR: "{{ paths.etc }}/php/{{ series }}/conf.d"

  # ---------- Serviço (opcional) ----------
  service:
    kind: pool                   # none | daemon | pool | oneshot | gui
    # kind: pool → gerenciado pelo forge-fcgi (só faz sentido para PHP)
    pool:
      exe: php-cgi.exe
      args: ["-b", "127.0.0.1:{{ port }}", "-c", "{{ paths.etc }}/php/{{ series }}/php.ini"]
      protocol: fastcgi
      port_range: php-pools      # nome de faixa definida em forgebox.toml
      scaling: { min: 2, max: 12, idle_timeout: 120s }
      recycle: { max_requests: 500, max_rss_mb: 256, max_age: 1h }
      env: { PHP_FCGI_MAX_REQUESTS: "{{ recycle.max_requests }}" }
      health: { fastcgi: "/_forgebox/health.php" }

  # ---------- Descoberta de instalação manual ----------
  discover:
    match_files: [php.exe, php-cgi.exe]
    version_command: "php.exe -r \"echo PHP_VERSION;\""
    variant_detect:
      ts: { command: "php.exe -i", regex: "Thread Safety => (enabled|disabled)", map: { enabled: ts, disabled: nts } }

  # ---------- Atualização ----------
  update:
    policy: auto-patch           # manual | notify | auto-patch | auto-minor | auto
    retention: 1                 # versões anteriores mantidas por série para rollback
    check_interval: 6h
    migrate:                     # o que levar da versão antiga para a nova na mesma série
      - copy_dir: { from: "ext/", to: "ext/", only_missing: true }   # extensões adicionadas pelo usuário (Xdebug etc.)

  # ---------- UI ----------
  ui:
    color: "#777BB4"
    actions:                     # botões no card da tool/instance
      - { label: "php.ini", icon: file-text, open: config:php_ini }
      - { label: "Terminal com esta versão", icon: terminal, shell: { path_prepend: ["{{ paths.version }}"] } }
      - { label: "Info (phpinfo)", icon: info, browse: "http://localhost/_forgebox/phpinfo?v={{ version }}" }
    badges: ["{{ variant.ts }}", "{{ variant.compiler }}"]
```

### 1.1 `service.kind`
| kind | Significado | Exemplos |
|---|---|---|
| `none` | Não é processo (biblioteca, CLI) | composer, git, php (o CLI), python |
| `daemon` | Processo de longa duração gerenciado pelo supervisor | nginx, httpd, mysqld, postgres, mailpit, memurai, minio, meilisearch |
| `pool` | Pool FastCGI gerenciado por `forge-fcgi` | php |
| `oneshot` | Executa e termina; usado em `post_install`/ações | `mysqld --initialize`, `pg_ctl initdb` |
| `gui` | App gráfico lançado sob demanda, fora do Job | heidisql, dbeaver, notepad++ |

### 1.2 `service.daemon` (referência)
```yaml
service:
  kind: daemon
  multi_instance: true           # permite mysql "default" e "legacy" com data dirs/portas próprias
  exe: bin/mysqld.exe
  args: ["--defaults-file={{ instance.config.my_ini }}", "--console"]
  cwd: "{{ paths.version }}"
  env: {}
  console_group: true            # CREATE_NEW_PROCESS_GROUP (para CTRL_BREAK)
  ports:
    - { name: main, default: 3306, env: null, range: databases }
  check: { command: "bin/mysqld.exe --defaults-file={{ instance.config.my_ini }} --validate-config" }
  start_timeout: 45s
  health:
    tcp: "127.0.0.1:{{ ports.main }}"      # ou http: {url, expect_status} ou command: {cmd, exit_code}
    interval: 5s
    success_threshold: 1
    failure_threshold: 3
  stop:
    command: "bin/mysqladmin.exe --port={{ ports.main }} -uroot shutdown"
    timeout: 20s
    then: ctrl-break               # próximo passo se o comando não parar
  reload: null                     # ou { command: "...", signal: ... }
  restart_policy: { on: failure, max: 3, window: 60s }
  logs:
    - { label: Error log, path: "{{ paths.logs }}/mysql/{{ instance.name }}/error.log", format: mysql }
  data:
    dir: "{{ paths.data }}/mysql/{{ instance.name }}"
    init:                          # inicialização de data dir na 1ª vez
      command: "bin/mysqld.exe --defaults-file={{ instance.config.my_ini }} --initialize-insecure"
  depends_on: []
  autostart: true
```

### 1.3 `config.quick_settings[].type`
`toggle`, `select`, `number`, `text`, `path`, `port`, `slider{min,max,step}`, `multi` (lista de checkboxes). Cada um sabe gravar em `format: ini | conf-directive | yaml | json | args`. Para `conf-directive` (Apache/Nginx), o motor edita só dentro do bloco gerenciado do arquivo.

### 1.4 Contexto de template nos manifestos
`{{ version }}`, `{{ series }}`, `{{ variant.<key> }}`, `{{ paths.root|bin|etc|data|logs|tmp|www|certs|version }}`, `{{ instance.name|ports.<n>|config.<id>|data_dir }}`, `{{ port }}` (em pools), `{{ prefs.<chave> }}`. Qualquer outra chave é erro de lint.

---

## 2. `kind: Extension`

```yaml
apiVersion: forgebox.dev/v1
kind: Extension
metadata: { id: xdebug, name: Xdebug, category: devtool, icon: xdebug.svg }
spec:
  extends: php
  compat:                        # resolvido contra a ToolVersion alvo
    - { php: "~8.3", variant: { ts: nts, compiler: vs17 }, version: "3.4.*" }
    - { php: "~8.4", variant: { ts: nts, compiler: vs17 }, version: "3.4.*" }
  source:
    providers:
      - type: http
        url: "https://xdebug.org/files/php_xdebug-{version}-{php_series}-{compiler}-{ts_flag}-x86_64.dll"
        vars: { ts_flag: { nts: "nts", ts: "" } }
        checksum: { type: sha256, url: "https://xdebug.org/files/{file}.sha256" }   # ilustrativo
  install:
    target: "{{ paths.version }}/ext/php_xdebug.dll"
    post_install:
      - render: { template: "php/xdebug.ini.j2", to: "{{ paths.etc }}/php/{{ series }}/conf.d/20-xdebug.ini" }
  config:
    quick_settings:
      - { key: mode, ini: "xdebug.mode", type: multi, options: [off, debug, develop, coverage, profile, trace], default: [debug, develop] }
      - { key: start_with_request, ini: "xdebug.start_with_request", type: select, options: [yes, trigger, no], default: trigger }
      - { key: client_port, ini: "xdebug.client_port", type: port, default: 9003, reserve: true }
  toggle: { enable: "zend_extension=php_xdebug.dll", disable: ";zend_extension=php_xdebug.dll", apply: reload }
```

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
      - { create_database: { instance: mysql/default, name: "{{ site.slug }}" } }
      - { run: "php artisan key:generate", cwd: "{{ dir }}" }
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
    host: loja.test
    aliases: [admin.loja.test]
    root: public
    driver: laravel
    server: nginx                # apache | nginx | caddy | frankenphp
    php: "8.3"                   # série ou versão exata "8.3.36"
    php_ini: { memory_limit: 1G, upload_max_filesize: 256M }
    xdebug: trigger
    secure: true
  services:
    - { tool: mysql, series: "8.4", instance: default, database: loja }
    - { tool: memurai }
    - { tool: mailpit }
  runtimes:
    - { tool: nodejs, version: "22" }       # usado por `forge shell` e pelos processes abaixo
  processes:                     # gerenciados pelo supervisor quando o site está "up"
    vite:   { cmd: "npm run dev -- --port $PORT", port: 5173, proxy_path: null }
    queue:  { cmd: "php artisan queue:work", restart: always }
    sched:  { cmd: "php artisan schedule:work" }
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
  tools:
    - { id: php, path: tools/php.yaml, sha256: "...", categories: [runtime], latest: "8.4.14" }
    - { id: nginx, path: tools/nginx.yaml, sha256: "..." }
  stacks:
    - { id: laravel, path: stacks/laravel.yaml, sha256: "..." }
  signature: { type: minisign, key_id: "RWR...", sig: "..." }
```
- Registries de terceiros: `forge registry add https://.../index.yaml` (exibidos com badge "terceiro"; manifestos com `post_install.run` fora da própria tool são recusados).
- Manifesto **embutido no pacote**: se o zip da tool contém `forgebox.yaml` na raiz, `forge tool add-local <zip>` instala direto. Permite que qualquer autor distribua sua ferramenta "pronta para Forgebox" sem passar pelo registry.

---

## 6. Regras de lint (`cargo xtask lint-registry`)
1. `id` único; `apiVersion`/`kind` válidos; `platforms` não vazio.
2. Todo provider `http` precisa de `checksum` ou `releases[].sha256`.
3. `service.ports[].range` deve existir em faixas conhecidas.
4. Templates referenciados devem existir em `forge-render/templates/` ou no próprio registry (`templates/` ao lado do yaml).
5. Variáveis de template desconhecidas = erro.
6. `post_install.run` só pode invocar executáveis dentro de `{{ paths.version }}`.
7. Ícones em SVG ≤ 20 KB.

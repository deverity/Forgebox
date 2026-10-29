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
    shims: [php, php-cgi, phpdbg, pear, pecl]    # bin/shims/<nome>.cmd → <nome>.exe|.com|.cmd|.bat da versão do projeto ou de current
    # forma objeto: { name: composer, file: composer.phar, interpreter: php } → bin/shims/php.cmd <file> <args>
    version_files: [".php-version"]   # arquivos do projeto (diretório atual e acima) que escolhem a versão nos shims
    project_key: spec.site.php        # chave do forgebox.yaml (kind: Project) com a versão; spec.runtimes[] com `tool: <id>` vale sempre
    verify:                      # roda na versão recém-extraída, antes do move para bin/ (docs/03 §5.1)
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
        overlay: "{{ paths.etc }}/php/{{ series }}/conf.d/90-forgebox-settings.ini"   # quick settings vão aqui, não no php.ini (§1.3)
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
      dir: ext                   # pasta dos arquivos de extensão, relativa à versão (padrão `ext`). Item de
                                 # defaults_on sem arquivo ali é embutido naquele build (PHP 8.4 Windows: bcmath)
                                 # ou ausente: não entra no overlay (evita o aviso "Unable to load dynamic library")
      defaults_on: [curl, mbstring, openssl, pdo_mysql, pdo_sqlite, pdo_pgsql, fileinfo, gd, intl, zip, sodium, exif, bcmath]
      zend_extensions: [{ name: opcache, constraint: "<8.5" }]  # PHP 8.5+ embute o OPcache
    # zend_extensions: itens `nome` ou `{ name, constraint }`; `constraint` usa a mesma sintaxe de
    # versioning.channels.*.constraint (scheme da tool) e o item só é emitido quando casa com a versão
    # instalada (ausente = sempre). Resolvido por ExtensionsConfig::zend_extensions_for.
    env:                         # variáveis injetadas nos processos desta tool e nos `processes:` que usam esta tool como `runtime:` (ex. nodejs: NODE_EXTRA_CA_CERTS, npm_config_prefix; docs/06 §4.4). Variáveis em tempo de execução: version, series, tool, paths.<dir> (sem paths.version)
      PHPRC: "{{ paths.etc }}/php/{{ series }}"
      PHP_INI_SCAN_DIR: "{{ paths.etc }}/php/{{ series }}/conf.d"

  # ---------- Serviço (opcional) ----------
  service:
    kind: pool                   # none | daemon | pool | oneshot | gui; cada kind com processo tem seu bloco (daemon:, pool:, oneshot:, gui:)
    # kind: pool → gerenciado pelo forge-fcgi (só faz sentido para PHP)
    pool:
      exe: php-cgi.exe
      args: ["-b", "127.0.0.1:{{ port }}", "-c", "{{ paths.etc }}/php/{{ series }}/php.ini"]  # {{ port }}: um por worker, preenchido pelo pool; exatamente um `<ip de loopback>:{{ port }}`
      protocol: fastcgi
      port_range: php-pools      # nome de faixa definida em forgebox.toml (regra 3)
      scaling: { min: 2, max: 12, idle_timeout: 120s }
      recycle: { max_requests: 500, max_rss_mb: 256, max_age: 1h }
      env: { PHP_FCGI_MAX_REQUESTS: "{{ recycle.max_requests }}" }  # o forge-fcgi sobrescreve com um teto acima de max_requests (docs/03 §2.1)
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
    logs:                            # a pasta de cada arquivo (dentro da raiz) é criada pelo daemon antes de cada start
      - { label: Error log, path: "{{ paths.logs }}/mysql/{{ instance.name }}/error.log", format: mysql }
    data:
      dir: "{{ paths.data }}/mysql/{{ instance.name }}"
      init:                          # inicialização de data dir na 1ª vez
        command: "bin/mysqld.exe --defaults-file={{ instance.config.my_ini }} --initialize-insecure"
                                     # (uma instância importada do Laragon aponta para o data dir de lá: `settings`
                                     #  `forgebox.external_data_dir`, ADR-0031; sem a marca vale `data.dir`)
    depends_on: []
    autostart: true
```

#### `logs[].access` (access log estruturado)
Todo log com `format: forgebox-json` **declara** `access` (regra 1, `missing_access`): é o mapeamento que o `forge-metrics` ([14](14-observability-metrics.md) §3.1) e a rotação usam para ler a linha JSON sem conhecer o id da tool. `access` em log de outro `format` é erro.

```yaml
logs:
  - label: Access log (JSON)
    path: "{{ paths.logs }}/nginx/access.json"
    format: forgebox-json
    access:
      ts:   { pointer: /t, unit: s, start: false }   # unit: s | ms | us | ns; start: true = instante em que a requisição chegou (Apache), fim = ts + duration
      host: /host                                    # string; pode ter porta
      status: /s                                     # número ou string numérica
      duration: { pointer: /rt, unit: s }
      upstream_duration: { pointer: /ut, unit: s, multi: sum }   # multi: sum (padrão) | last, para "0.010, 0.020 : 0.003"; omitido se o servidor não registra
      bytes: /b                                      # opcionais: bytes, method, uri, upstream
      method: /m
      uri: { pointer: /u, strip_query: false }       # strip_query: true quando o valor traz a query string (Caddy)
      upstream: /up
      lenient_escapes: false                         # true: o servidor escreve \xhh/\v (Apache) e o parser reescreve antes de decodificar
      rotate:
        max_size: 50MB                               # B | KB | MB | GB (binários); obrigatório em rename-reopen
        keep: 5                                      # arquivos rotacionados mantidos (padrão 5)
        strategy: rename-reopen                      # rename-reopen | self | none (padrão)
        reopen: { command: "nginx.exe -s reopen ..." }   # ou { signal: ctrl-c | ctrl-break }; obrigatório (e só válido) em rename-reopen
```

- Cada `pointer` é um JSON Pointer (RFC 6901) para um membro de objeto: começa com `/`, sem token vazio, `~` só como `~0`/`~1`. Um ponteiro não pode ser igual nem prefixo de outro (o parser usa uma trie): `/request` e `/request/host` juntos são erro.
- `strategy`: `rename-reopen` = o daemon renomeia (`access.json.1`...) e pede ao servidor que reabra o arquivo (`reopen`, com os mesmos `-p`/`-c` do `reload`); `self` = o próprio servidor rotaciona, configurado no template (Caddy: `roll_size`/`roll_keep` em `Caddyfile.j2`, em sincronia com `max_size`/`keep`); `none` = sem rotação (Apache, **provisório** até M2-E5-T12).
- Manifestos oficiais: nginx (`/t` s, `/rt` s, `/ut` s somado, `rename-reopen` com `nginx.exe -s reopen`), Apache (`/t` ms com `start: true`, `/rt_us` µs, host com porta, `lenient_escapes`, rotação `none`) e Caddy (`/ts`, `/duration`, `/request/host`, `/request/uri` com `strip_query`, `self`).
- Erros de schema (ponteiro ausente, unidade desconhecida) e de validação (`rename-reopen` sem `reopen`, ponteiro inválido) saem como `arquivo:linha:coluna` no `lint-registry`.

### 1.3 `config.quick_settings[]`
`type`: `toggle`, `select`, `number`, `text`, `path`, `port`, `slider{min,max,step}`, `multi` (lista de checkboxes). O `format` do arquivo (`config.files[].format`: `ini | conf-directive | yaml | json | args | text`) decide como o valor é gravado; o alvo dentro do arquivo é **um** destes campos, o do formato do arquivo (sem nenhum, o alvo é a própria `key`):

| Formato | Campo | Alvo | Como é escrito |
|---|---|---|---|
| `ini` | `ini` | `chave` ou `seção/chave` (`mysqld/max_connections`); o ponto faz parte da chave (`opcache.enable`) | troca a **última** ocorrência (vale a última no PHP e no MySQL); ausente: no fim da seção (seção criada no fim se faltar); sem seção: no fim |
| `conf-directive` | `directive` | nome da diretiva (sem caixa, como no Apache/PostgreSQL) | troca os argumentos da última ocorrência no nível mais raso de `{}`/`<Seção>`, mantendo `nome valor`/`nome = valor`, `;`, aspas e comentário; ausente: no nível de cima, no estilo das outras linhas (diretivas de contexto aninhado, como as do `http {}` do nginx, ficam no template) |
| `yaml`, `json` | `pointer` | caminho com pontos (`server.port`) de mapeamentos/objetos | troca o valor no lugar; chaves ausentes são criadas |
| `args` | `arg` | flag (`--smtp`), um argumento por linha | a linha seguinte à flag ou o `--flag=valor`; ausente: acrescentada; booleano nativo = presença da flag |
| `text` | — | — | sem quick settings |

`text` é editável, mas sem quick settings. Valores são escritos **só em texto do Forgebox** ([ADR-0042](adr/0042-quick-settings-storage-and-apply.md)): no corpo do bloco gerenciado (ou arquivo gerado) que o alvo de render produz para aquele `path` — o `my.ini` e o `nginx.conf` saem do template já com os valores, e o hash do bloco os cobre —; no `overlay` do arquivo, quando declarado; ou, num arquivo que nenhum alvo renderiza, num bloco gerenciado `settings` no fim dele (`json`/`args`, sem comentários, exigem overlay ou alvo). Fora disso o Forgebox não escreve.

`config.files[].overlay`: arquivo 100 % do Forgebox (cabeçalho `generated by forgebox`, mesmo formato) que a tool lê **depois** do arquivo, para que o arquivo do usuário nunca seja editado. PHP: `{{ paths.etc }}/php/{{ series }}/conf.d/90-forgebox-settings.ini`, lido por `PHP_INI_SCAN_DIR` depois do `php.ini` e do `10-forgebox.ini`. Fica em `{{ paths.etc }}` (lint, regra 6).

| Campo | Vale para | Regra |
|---|---|---|
| `options` | `select`, `multi` | obrigatório nesses tipos; proibido nos demais |
| `on` / `off` | `toggle` | valor gravado; ausentes = booleano nativo do formato (`On`/`Off` em ini, `on`/`off` em conf-directive, `true`/`false` em yaml/json, presença da flag em args) |
| `min` / `max` / `step` | `number`, `slider` | `slider` exige `min` e `max`; `min <= max`; `step > 0` (o valor é `min` + n × `step`) |
| `reserve` | `port` | reserva a porta no alocador (Xdebug 9003) |
| `default` | todos | `toggle`: true/false/on/off; `select`: um de `options`; `multi`: lista contida em `options`; `port`: 1–65535; `number`/`slider`: dentro de min/max |
| `apply` | todos | `reload` \| `restart` (padrão) \| `none` |
| `also_set` | todos | outros alvos **do mesmo arquivo**, na sintaxe do alvo (`post_max_size`, `mysqldump/max_allowed_packet`), gravados com `{{ value }}` = o texto escrito |
| `file` | todos | obrigatório numa Tool (o `config.files[].id` que o ajuste escreve) |

**Valores** (M2-E2-T7): a fonte é o SQLite — `instances.settings` para instâncias e `tool_series_settings` por `(tool, série)` para os pools de PHP (`php@8.4`, sobrevive a updates de patch) —, guardados como o texto canônico de `services.set`. O valor **efetivo** de cada ajuste é o guardado ou, sem ele, o `default`, e todos os efetivos são escritos (padrões inclusive), então o arquivo tem o que `services.settings` mostra. Os templates que renderizam o arquivo inteiro recebem os mesmos valores no contexto (`instance.settings.<key>`, `php.settings.<key>`, já no texto do arquivo: `On`, `1G`).

Na API ([ADR-0041](adr/0041-ipc-additions-m2-core-screens.md)), `services.settings` devolve cada quick setting com `kind`, opções, limites, `apply`, `also_set` e o valor atual/padrão tipado pelo `type` (toggle → booleano; number/slider/port → número; multi → lista; select/text/path → texto), os arquivos (o do usuário com overlay vem `managed: false`; o overlay vem com o papel `<id>.overlay`) e as portas nomeadas. `services.set` recebe o **texto** de cada um (`true`/`false` — também `on`/`off`/`1`/`0` ou os textos de `on`/`off` —, números decimais, itens de `multi` separados por `,`, opções sem diferença de caixa; vazio = volta ao padrão) e as portas como `ports.<nome>=<porta>`. Tudo é validado antes de qualquer escrita (chave desconhecida ou valor fora de `options`/`min`/`max`/`step` → `invalid_input` com a chave; bloco gerenciado editado à mão → `conflict`; porta ocupada → `port_in_use` com `pid`/`exe`; reservada para outro → `conflict`); depois o job guarda, renderiza (`config.rendered`) e aplica o modo mais forte dos ajustes alterados: `reload` = recycle do pool (PHP) ou o `reload` do serviço; `restart` = pelo supervisor, validando a configuração antes de parar; `ports.<nome>` move a reserva (`PortAllocator::reserve`), re-renderiza e reinicia a instância com o spec novo ([03](03-engine.md) §3.4, §4).

### 1.4 Contexto de template nos manifestos
Strings dos manifestos só **substituem variáveis**: `{{ caminho.da.variavel }}`, com filtros opcionais (`{{ paths.etc | posix_path }}`). Literais, operadores e blocos `{% %}` são erro (regra 5). Variáveis permitidas:

| Onde | Variáveis |
|---|---|
| Tool e Extension (qualquer campo de `spec`) | `{{ version }}`, `{{ series }}`, `{{ variant.<key> }}` (key declarada em `versioning.variants`; em Extension, as da tool alvo), `{{ paths.root\|bin\|etc\|data\|logs\|tmp\|www\|certs\|version }}`, `{{ instance.name\|data_dir }}`, `{{ instance.ports.<n> }}` (porta declarada em `service.daemon.ports`), `{{ instance.config.<id> }}` (id de `config.files`), `{{ prefs.<chave> }}` |
| `spec.install` (Tool e Extension) | só `{{ version }}`, `{{ series }}`, `{{ variant.<key> }}` e `{{ paths.<dir> }}`: `install` roda **uma vez por versão, antes de existir instância** (`instance.*` e `prefs.*` são erro de lint, regra 5). Pastas e arquivos por instância (`logs/mysql/<instância>/`, `my.ini`) vêm do alvo de render e de `service.daemon.logs` no start |
| `config.quick_settings[]` | + `{{ value }}` (texto escrito do valor escolhido, em `also_set`) |
| `spec.database` | + `{{ database }}` (todos os campos menos `client`), `{{ charset }}` (só `queries.*`), `{{ sql }}` (só `client.args`); `{{ instance.ports.<n> }}` como nas demais (§1.8) |
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
| `install` | `archive` deduzido do arquivo, `strip_root: auto`, `layout: versioned`, `current_link: true`, `path: []`, `shims: []`, `version_files: []`, sem `project_key` |
| `config.files[].editable` | `true` |
| `quick_settings[].apply` / `toggle.apply` | `restart` |
| `service.kind` | `none` |
| `service.daemon` | `console_group: true`, `start_timeout: 30s`, `stop: {timeout: 15s, then: ctrl-break}`, `restart_policy: {on: failure, max: 3, window: 60s}`, `health.interval: 5s`, thresholds 1/3, `http.expect_status: 200`, `autostart: false`, `multi_instance: false` |
| `service.pool` | `protocol: fastcgi`, `port_range: php-pools`, `scaling: {min: 2, max: 12, idle_timeout: 120s}`, `recycle: {max_requests: 500, max_rss_mb: 256, max_age: 1h}` |
| `update` | `policy: notify`, `retention: 1`, `check_interval: 6h` |
| `processes.*` (Project) | `restart: failure`, `autostart: true`, `proxy: false` |

### 1.8 `spec.database` (capacidade de banco de dados)
Bloco opcional e aditivo ([ADR-0040](adr/0040-e3-api-terminal-db-mail-logs.md)): o daemon lista, cria e remove bancos, exporta/importa e monta strings de conexão **só a partir do manifesto**, sem código por tool. `db.*` aparece para toda instância cuja tool declara o bloco.

```yaml
database:
  protocol: mysql                 # mysql | postgresql: só a família de aspas/validação de identificadores
  port: main                      # nome de service.daemon.ports[] (regra 1, invalid_reference)
  admin_user: root
  default_charset: utf8mb4        # valor de {{ charset }} quando o chamador não informa ([A-Za-z0-9_]{1,32})
  system_databases: [information_schema, mysql, performance_schema, sys]   # escondidos em db.list
  client:                         # roda UMA instrução; imprime linhas separadas por TAB, sem cabeçalho
    exe: bin/mysql.exe            # relativo à pasta da versão (regra 6: sem .., caminho absoluto ou variáveis)
    args: ["--protocol=tcp", "--host=127.0.0.1", "--port={{ instance.ports.main }}", "--user=root",
           "--batch", "--skip-column-names", "--execute={{ sql }}"]
  queries:                        # SQL ANSI (information_schema) sempre que a família permite
    list: "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name"   # nome na 1ª coluna
    create: "CREATE DATABASE `{{ database }}` CHARACTER SET {{ charset }}"
    drop: "DROP DATABASE `{{ database }}`"
  dump: { exe: bin/mysqldump.exe, args: ["--port={{ instance.ports.main }}", "--user=root", "{{ database }}"] }   # stdout
  restore: { exe: bin/mysql.exe, args: ["--port={{ instance.ports.main }}", "--user=root", "{{ database }}"] }    # stdin
  shell: { exe: bin/mysql.exe, args: ["--port={{ instance.ports.main }}", "--user=root"] }                        # db.shell
  url: "mysql://root@127.0.0.1:{{ instance.ports.main }}/{{ database }}"
  env:                            # variáveis do .env do projeto
    DB_CONNECTION: mysql
    DB_PORT: "{{ instance.ports.main }}"
    DB_DATABASE: "{{ database }}"
```

- **Variáveis novas, válidas só neste bloco:** `{{ database }}` (nome do banco), `{{ charset }}` (só em `queries.*`) e `{{ sql }}` (só em `client.args`). Fora dele continuam erro de regra 5 (`unknown_template_variable`); `{{ instance.ports.<n> }}` usa as portas declaradas, como no resto do manifesto.
- `queries.create` e `queries.drop` precisam de `{{ database }}`; `client.args` precisa de `{{ sql }}`. `dump`, `restore` e `shell` são opcionais (sem eles a capacidade correspondente some da UI).
- **Segurança:** o consumidor valida `database` (`^[A-Za-z0-9_]{1,64}$`) e `charset` (`^[A-Za-z0-9_]{1,32}$`) **antes** de substituir, e o valor substituído nunca é reexaminado. `forge_manifest::database::{DbVars, DatabaseSpec::render_*}` implementa isso e devolve `(exe, argv)` exatos; o daemon só executa o resultado. Nomes diferenciam maiúsculas (docs/15).
- PostgreSQL não expõe bancos em `information_schema`; `queries.list` lê `pg_database` (sem templates), e `psql` usa `-At -F "\t"` (TAB real em YAML de aspas duplas).
- Manifestos oficiais: `mysql`, `mariadb` (família `mysql`, clientes `mariadb`/`mariadb-dump`) e `postgresql`.

### 1.9 `spec.mail` (capacidade de captura de e-mail)
```yaml
mail:
  api: mailpit-v1                 # família de API que o adaptador do daemon fala (hoje só mailpit-v1: /api/v1)
  http_port: http                 # nomes de service.daemon.ports[] (regra 1, invalid_reference)
  smtp_port: smtp
  env:                            # variáveis MAIL_* do .env do projeto
    MAIL_MAILER: smtp
    MAIL_HOST: "127.0.0.1"
    MAIL_PORT: "{{ instance.ports.smtp }}"
```
O daemon escolhe o adaptador por `api`, nunca pelo id da tool. Manifesto oficial: `mailpit`.

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

1. `id` único (Tool + Extension em `tools/`, Stack em `stacks/`), arquivo `<id>.yaml` na pasta do seu `kind`; `apiVersion`/`kind` válidos e schema respeitado (chave desconhecida, tipo errado); `platforms` não vazio; referências a outros manifestos existem (`depends_on`, `suggests`, `replaced_by`, `extends` → Tool, ferramentas e extensions de Stack); `index.yaml` lista exatamente os manifestos existentes, com `id`, `kind` e `sha256` corretos. Todo log `forgebox-json` exige `access` (`missing_access`, §1.2). Demais validações semânticas (variantes, restrições, referências internas, URLs, hosts) também caem aqui, inclusive as de quick settings (o campo de alvo é o do formato do arquivo — `ini`, `directive`, `pointer` ou `arg` —, `ini` é `chave` ou `seção/chave`, `arg` começa com `-`, alvos e chaves de `also_set` sem espaço, aspas, `=`, `;` ou `#`, arquivo `text` sem quick settings nem overlay, quick setting de Tool com `file`) e as de `install`: nomes de shim e de `version_files` em `[A-Za-z0-9._-]` sem repetição (`forgebox.yaml` não é `version_files`: use `project_key`), `shims[].file` relativo com `/` e sem `..`, `shims[].interpreter` = outro shim e exige `file`, `project_key` pontuado em `[a-z0-9_]` começando por `spec.`.
2. Provider sem checksum upstream (`http`/`github-release` sem `checksum`, `nginx-org`, `mysql`, `postgresql-edb`, `python-embed`) precisa de `releases[].sha256` em todas as releases.
3. `service.daemon.ports[].range` e `service.pool.port_range` devem existir nas faixas conhecidas (`forge_core::config`, [03](03-engine.md) §4).
4. Templates referenciados (`post_install[].render.template`) devem existir em `forge-render/templates/` ou no próprio registry (`<registry>/templates/<caminho>`).
5. Variáveis de template desconhecidas (§1.4) ou expressões que não são variáveis = erro. `{{ sql }}`, `{{ database }}` e `{{ charset }}` só valem dentro de `spec.database` (§1.8, nos campos indicados); em qualquer outro lugar são `unknown_template_variable`.
6. `post_install.run` só pode invocar executáveis dentro de `{{ paths.version }}` (caminho relativo, resolvido lá e nunca via PATH; sem `..`, caminho absoluto ou variáveis); destinos de `copy`/`render`/`mkdir` começam com `{{ paths.<dir> }}` e não saem dele; `config.files[].overlay` começa com `{{ paths.etc }}` e não sai dele; `Extension.install.target` começa com `{{ paths.version }}`; registries de terceiros não podem declarar `run`. Os `exe` de `spec.database` (`client`, `dump`, `restore`, `shell`) seguem a mesma ideia: caminho relativo à pasta da versão, sem `..`, caminho absoluto ou variáveis (`exe_outside_version`).
7. Ícones em SVG ≤ 20 KB (arquivo em `registry/icons/` ou URL `https://` para `.svg`), sem `<script>`; tudo em `registry/icons/` é `.svg`.

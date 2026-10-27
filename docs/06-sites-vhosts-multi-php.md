# 06 — Sites, vhosts, multi-PHP, reverse proxy, DNS e SSL

## 1. Modelo de Site

```
Site {
  id, slug,                       # slug derivado do host (loja-test)
  host: "loja.test",
  aliases: ["admin.loja.test", "loja.local"],
  kind: php | static | proxy | app,
  root: "C:\www\loja\public",     # para php/static
  driver: laravel | symfony | wordpress | cakephp | drupal | magento | craft | statamic | generic-php | static | spa,
  server: apache | nginx | caddy | frankenphp,   # servidor frontal deste site (padrão = prefs.default_server)
  php: { series: "8.3" | exact: "8.3.36", pool_overrides?: {...} },   # só kind=php
  upstream: "http://127.0.0.1:3000",  # só kind=proxy
  secure: true,                   # HTTPS com cert da CA local
  http_redirect: true,            # 80 → 443
  env: { APP_ENV: local },        # FastCGI params / env para processes
  xdebug: inherit | off | trigger | always,
  parked: bool,                   # criado automaticamente a partir de um diretório parked
  enabled: bool,
  processes: [...],               # ver §6
  created_at, updated_at
}
```

### 1.1 Origens de um site
1. **Parked directories** (como `www/` do Laragon ou `herd park`): o watcher cria um site `nome.test` para cada subpasta. Driver detectado automaticamente (ex. existe `artisan` → Laravel → root `public/`). Pode haver vários diretórios parked (`C:\www`, `D:\clientes`), cada um com TLD próprio opcional.
2. **Link explícito**: `forge site add <dir> [--host x.test]` (como `valet link`). Para projetos fora dos parked dirs.
3. **Arquivo `forgebox.yaml` do projeto** (`kind: Project`): `forge up` dentro da pasta.
4. **Proxy**: `forge proxy add app.test http://127.0.0.1:3000 --ws`.

### 1.2 Drivers
Driver = detecção + regras (root, rewrites, variáveis). Implementados em Rust (`forge-sites/src/drivers/`), não em PHP como no Valet, para serem rápidos e funcionarem com qualquer servidor. Cada driver produz um `VhostModel` neutro que `forge-render` traduz para Apache/Nginx/Caddy:

```
VhostModel {
  kind: php | static | proxy,
  hosts, root, index: [index.php, index.html],
  php_upstream: "php_8_3" | None,
  try_files: ["$uri", "$uri/", "/index.php?$query_string"],
  static_locations: [...], deny: ["/\\.ht", "/.env"],
  headers, client_max_body: "256M",
  ssl: { cert, key } | None, http_redirect: bool,   # http→https
  proxy: { upstream, websocket, start_on_visit, read_timeout_secs } | None,   # kind: proxy
  env: { NOME: valor },                  # fastcgi_param/SetEnv; sem `$` nem `%{`
  fallback_resource,                     # derivado do último try_files (Apache)
  extra: { apache: "...", nginx: "...", caddy: "..." }   # escape hatch por servidor (do usuário)
}
```

### 1.3 Implementação (M1-E4-T2)

- **Persistência:** `Site` e o repositório (`SiteRepo`) vivem em `forge-core::store::sites` (o store não depende de `forge-sites`); `forge-sites::site` os reexporta e acrescenta `SiteDraft` (entrada de `site.create`), `normalize_host` e `slug_for_host`. Host e aliases são únicos **globalmente** (um alias não pode ser host de outro site).
- **Validação de host:** minúsculo, `[a-z0-9.-]`, labels de 1–63 chars sem `-` nas pontas, terminando em um TLD configurado (`SiteRules::tlds`, padrão `test`). Maiúsculas são **normalizadas com aviso** (`sites.host_lowercased`), nunca gravadas. Aliases seguem a mesma sintaxe sem a restrição de TLD (`loja.local`). Slug = host com `.` → `-`.
- **Drivers:** `Driver { id, kind, detect(dir) -> Option<Confidence>, root(dir), tune(&mut VhostModel), vhost(site, ctx) }`. Maior `Confidence` vence; empate vai ao primeiro da lista (`laravel, symfony, wordpress, cakephp, drupal, magento, generic-php, spa, static`). Pasta que ninguém reconhece → `generic-php` com a pasta como root. Override manual (`forgebox.yaml spec.site.driver`) ignora a detecção.

| Driver | Marcadores (alta confiança) | Root | `try_files` |
|---|---|---|---|
| laravel | `artisan` + `public/index.php` | `public/` | `$uri $uri/ /index.php?$query_string` |
| symfony | `bin/console` + `public/index.php` | `public/` | `$uri /index.php?$query_string` |
| wordpress | `wp-config.php` ou `wp-load.php` (Bedrock: `web/wp-config.php`) | raiz (ou `web/`) | `$uri $uri/ /index.php?$args`; nega `.php` em `wp-content/uploads` |
| cakephp | `bin/cake.php` + `webroot/index.php` (CakePHP 2: `lib/Cake` + `app/webroot`) | `webroot/` | `$uri $uri/ /index.php?$args` |
| drupal | `core/lib/Drupal.php` (também sob `web/` ou `docroot/`) | raiz, `web/` ou `docroot/` | `$uri /index.php?$query_string`; nega extensões internas |
| magento | `bin/magento` + `pub/` | `pub/` | `$uri $uri/ /index.php?$query_string`; `MAGE_MODE=developer` |
| generic-php | `index.php` na raiz ou em `public/`, `public_html/`, `web/` (média); outro `.php` na raiz (baixa) | raiz ou subpasta com `index.php` | padrão |
| spa | `dist/` ou `build/` com `index.html`, sem `index.php` na raiz/`public/` | `dist/` ou `build/` | `$uri $uri/ /index.html` |
| static | `index.html`/`index.htm` na raiz, sem `.php` | raiz | `$uri $uri/ =404` |

- **Vhost (`vhost::build`):** `hosts = [host, "*.host", ...aliases]`; `php_upstream = php_<série com _>` (`php_8_3`) ou `php_<versão exata com _>` (`php_8_3_36`, pool próprio, §2.1); `deny` sempre inclui `/\.ht`, `/\.env`, `/\.git`; TLS em `certs/sites/<host>.crt|key` quando `secure`; `http_redirect` só com TLS; `extra` por servidor vem do contexto (texto do usuário, repassado sem alteração).
- **Parked (`parked.rs`):** `ParkedSync::reconcile` compara disco e store (cria, remove, atualiza root/driver); `ParkedWatcher` roda um reconcile na partida e após cada rajada de eventos do `notify`, com debounce de 500 ms (teto de 5 s). O watch **não é recursivo**: criar/apagar a pasta do projeto é visto na hora; mudança de driver dentro do projeto (ex. `artisan` aparece) é vista no próximo reconcile. Pastas iniciadas por `.` ou `_` são ignoradas, assim como as irmãs temporárias de uma conversão case-sensitive (`loja.cs-tmp`, `loja.cs-old`, [15](15-linux-parity-case-sensitivity.md) §3.1); nome da pasta vira label (`Minha Loja_2` → `minha-loja-2.test`). Site explícito com o mesmo host sempre vence (o implícito não é criado; criar o explícito substitui o implícito). Diretório parked ilegível (disco removido) não apaga seus sites.
- **`forgebox.yaml`:** `ProjectPlan::load(dir, rules)` valida com `forge-manifest` e resolve para `{ site: SiteDraft, php_ini, services, runtimes, processes, proxies: Vec<SiteDraft>, share }`; `site.root` deve ficar dentro do projeto; `php: "8.3"` é série e `"8.3.36"` é versão exata.

## 2. PHP por vhost (o recurso central)

### 2.1 Como funciona
- Cada **série** de PHP usada por pelo menos um site ativo tem um **PhpPool** (ver [03-engine.md §2](03-engine.md)). Se um site fixa versão exata (`8.3.36`), o pool é daquela versão exata; se fixa série (`8.3`), o pool segue a última patch instalada da série (e acompanha auto-update).
- O vhost renderizado aponta para o upstream do pool:

**Nginx** (`etc/nginx/sites/loja.test.conf`):
```nginx
# generated by forgebox
server {
    listen 127.0.0.1:443 ssl;
    http2 on;                        # nginx >= 1.25.1 (listen ... http2 está obsoleto)
    server_name loja.test *.loja.test admin.loja.test;
    root "C:/www/loja/public";
    include snippets/ssl-loja.test.conf;
    include snippets/php-common.conf;
    location ~ \.php$ {
        include fastcgi_params;
        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
        fastcgi_param FORGEBOX_SITE loja.test;
        fastcgi_pass php_8_3;            # ← upstream do pool da série 8.3
    }
}
```
`etc/nginx/upstreams/php_8_3.conf` (gerado e atualizado pelo pool a cada scale):
```nginx
upstream php_8_3 { least_conn; server 127.0.0.1:19083; server 127.0.0.1:19084; }
```

**Apache** (`etc/apache/sites/loja.test.conf`):
```apache
<VirtualHost 127.0.0.1:443>
    ServerName loja.test
    ServerAlias *.loja.test admin.loja.test
    DocumentRoot "C:/www/loja/public"
    Include "C:/Forgebox/etc/apache/snippets/ssl-loja.test.conf"
    <Directory "C:/www/loja/public">
        AllowOverride All
        Require all granted
    </Directory>
    <FilesMatch "\.php$">
        SetHandler "proxy:balancer://php_8_3/"
    </FilesMatch>
    ProxyFCGIBackendType GENERIC
    ProxyFCGISetEnvIf "reqenv('SCRIPT_FILENAME') =~ m#^/([A-Za-z]:/.*)$#" SCRIPT_FILENAME "$1"
    ProxyFCGISetEnvIf "true" FORGEBOX_SITE loja.test
</VirtualHost>
```
`etc/apache/upstreams/php_8_3.conf`:
```apache
<Proxy "balancer://php_8_3/">
    BalancerMember "fcgi://127.0.0.1:19083" enablereuse=off max=1 timeout=300
    BalancerMember "fcgi://127.0.0.1:19084" enablereuse=off max=1 timeout=300
    ProxySet lbmethod=bybusyness
</Proxy>
```
**Por que `ProxyFCGIBackendType GENERIC` + `ProxyFCGISetEnvIf ... SCRIPT_FILENAME`:** o `php-cgi` é o único backend no Windows. Com o tipo padrão (FPM) o `mod_proxy_fcgi` envia `SCRIPT_FILENAME=proxy:fcgi://127.0.0.1:19083/C:/…/index.php`; com `GENERIC` envia `/C:/…/index.php` (barra antes da letra do drive). Nos dois casos o `php-cgi` responde 404 `No input file specified.`; a regra `ProxyFCGISetEnvIf` remove a barra. `httpd -t` não detecta o problema.

**Por que não há reuso de conexão (nginx sem `fastcgi_keep_conn`/`keepalive`; Apache `enablereuse=off max=1`):** o `php-cgi` atende uma conexão por vez. Com keep-alive ele fica preso à conexão ociosa guardada pelo servidor e novas conexões para o mesmo worker esperam na fila do `accept` (medido: 62 s no nginx, estouro de 30 s no Apache, contra ~2,5 s sem reuso). Com mais requisições simultâneas que workers (AJAX/Livewire) isso trava o site.

Módulos necessários (habilitados pelo template): `proxy`, `proxy_fcgi`, `proxy_balancer`, `lbmethod_bybusyness`, `slotmem_shm`, `ssl`, `rewrite`, `headers`, `http2`. **Não usamos `mod_fcgid` nem `mod_php`** no modo padrão: o pool é nosso. Modo legado `mod_fcgid` existe como opção (`prefs.apache.php_mode = fcgid`) para quem precisa de `FcgidWrapper` por vhost.

**Caddy**:
```
loja.test, *.loja.test {
    root * C:/www/loja/public
    tls C:/Forgebox/certs/sites/loja.test.crt C:/Forgebox/certs/sites/loja.test.key
    php_fastcgi 127.0.0.1:19083 127.0.0.1:19084 { lb_policy least_conn }
    file_server
}
```

### 2.2 Trocar o PHP de um site
`forge site php loja.test 8.4` ou dropdown na UI:
1. Garante pool `8.4` ativo (instala se faltar, com confirmação).
2. Re-renderiza o vhost. 3. Reload do servidor. 4. Drena pool antigo se ficou órfão.
Tempo típico: < 2 s. Nenhum outro site é afetado.

Implementação: `forge_fcgi::swap::swap` ([03 §2.2](03-engine.md)); os passos 2–3 são o `UpstreamListener` injetado pelo daemon (`UpstreamChange::Swapped{from, to}`) e o passo 4 é `SwapOptions::drain_source` (o daemon passa `false` se outro site ainda usa o pool antigo). Medido com PHP real 8.3 → 8.4 sob carga: troca em ~2 s, zero requisições perdidas.

### 2.3 Versão de CLI vs versão web
- Web: cada site decide (pool).
- CLI global: `bin/php/current` (junction) → `forge use php 8.4`.
- CLI por projeto: `forge shell` lê `forgebox.yaml`/`.php-version` do projeto e monta PATH com a versão certa; o terminal integrado da UI faz o mesmo ao abrir "Terminal neste site". Shims opcionais (`bin/shims/php.cmd`) consultam `.php-version` no cwd antes de cair no `current`, replicando o comportamento de `fnm`/`asdf`.
- Shims (implementados em M1-E2-T4, `forge_provider::layout::shims`, [ADR-0025](adr/0025-layout-staging-shims-user-path.md)): um `.cmd` por item de `install.shims`, gerado (e regenerado de forma idempotente) a cada instalação da tool. Do diretório atual até a raiz do disco, o primeiro diretório que tiver um dos `install.version_files` (php: `.php-version`; nodejs: `.nvmrc`, `.node-version`) ou um `forgebox.yaml` que nomeie a versão (`install.project_key`, php: `spec.site.php`; ou um item `spec.runtimes[]` com `tool: <id>`) decide; no mesmo diretório, os arquivos de versão vêm antes do `forgebox.yaml`, e um arquivo vazio não decide. O arquivo de versão é lido pelo próprio `cmd.exe` (primeira palavra da primeira linha que não começa com `#`; texto ASCII/UTF-8 sem BOM; LF ou CRLF); o `forgebox.yaml` por `bin/shims/forgebox-project.ps1` (Windows PowerShell, iniciado só quando o arquivo existe). A seleção (`8.3`, `8.3.37`, `v22`, nome da pasta) é procurada numa tabela gravada no shim (pasta, versão, `major`, `major.minor`, com ou sem `v` → build preferido: mais nova, depois a ordem de variantes do manifesto). Versão pedida e não instalada → mensagem com o que está instalado e `exit 1` (nunca usa outra versão em silêncio). Sem seleção → `bin/<tool>/current`. Um shim com `interpreter` (composer: `{ name: composer, file: composer.phar, interpreter: php }`) chama `bin/shims/php.cmd composer.phar <args>`, então o `.php-version` do projeto vale também para o Composer. Conteúdo lido do projeto nunca é executado (só expansão atrasada do `cmd`). Limitação conhecida de `.cmd`: Ctrl+C mostra "Terminate batch job (Y/N)?".
- Ambiente dos shims e do `forge exec` (gate M1, [ADR-0032](adr/0032-m1-gate-integration.md)): antes de rodar uma versão, o shim chama o `forgebox-env.cmd` da pasta da versão (`bin/php/<versão>/forgebox-env.cmd`, gerado junto com os shims a partir do `config.env` do manifesto: `PHPRC`, `PHP_INI_SCAN_DIR`); os caminhos dentro da raiz são relativos ao próprio script (`%~dp0..\..\..\etc\php\8.4`), então funciona pela junction `current` e numa raiz portátil movida. Sem isso o `php` da CLI subia sem o `php.ini` do Forgebox (sem extensões, sem o bundle de CAs). A variável `FORGEBOX_<TOOL>_VERSION` (`FORGEBOX_PHP_VERSION=8.3`), quando definida, escolhe a versão antes de qualquer arquivo do projeto (lida só com expansão atrasada); `forge exec --php 8.3` a define, então `composer` → `php.cmd` usa o mesmo PHP do `PATH`. `forge env`/`forge exec` (`shell.env`) devolvem, além das pastas das versões, `bin/shims` no fim do `PATH` (comandos que só existem como shim, como `composer`) e as variáveis do `config.env` da versão escolhida; o `forge exec` procura o programa no `PATH` com o `PATHEXT` (acha `composer.cmd`). **Limitação:** quem põe `bin/php/current` direto no `PATH` (`[path] current_links`, sem shims) roda `php.exe` sem `PHPRC`; use `forge exec`, `forge shell` ou os shims.

## 3. Servidores web: Apache, Nginx, Caddy, FrankenPHP simultâneos
- `prefs.default_server` define quem escuta 80/443. Um segundo servidor pode rodar em portas alternativas (8080/8443) e sites escolhem `server:`; ou o frontal faz **proxy** para o outro (`server: apache` com Nginx frontal → Nginx proxy_pass para Apache em 8080). O Forgebox gera isso automaticamente quando `prefs.multi_server_mode = proxy`.
- Mudar o servidor frontal é uma operação: para o atual, re-renderiza todos os vhosts para o novo, sobe, verifica health, só então remove o antigo. Rollback automático se falhar.

## 4. Reverse proxy e apps (Node, Next.js, Vite, Bun, Deno, Python, Go, .NET)

### 4.1 Site proxy
`forge proxy add app.test http://127.0.0.1:3000 --ws --secure`
```nginx
server {
    listen 127.0.0.1:443 ssl; http2 on; server_name app.test;
    include snippets/ssl-app.test.conf;
    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection $connection_upgrade;
        proxy_set_header Host $host; proxy_set_header X-Forwarded-Proto https; proxy_set_header X-Forwarded-For $remote_addr;
        proxy_read_timeout 1h;          # HMR/WebSocket
        proxy_buffering off;
    }
}
```
Apache equivalente com `ProxyPass "/" "http://127.0.0.1:3000/" upgrade=websocket` (2.4.47+). Caddy: `reverse_proxy 127.0.0.1:3000`.

### 4.2 Processes (Procfile 2.0)
Um site (ou o `forgebox.yaml` do projeto) declara processos; o supervisor os gerencia como `App`:
```yaml
processes:
  web:   { cmd: "npm run dev -- --port $PORT", port: auto, proxy: true, runtime: nodejs@22 }
  queue: { cmd: "php artisan queue:work --tries=3", restart: always, runtime: php@8.3 }
```
- `$PORT` é alocado pelo Forgebox (faixa apps 3000–3999) e injetado; com `proxy: true` o host do site aponta para ele automaticamente → **Next.js em `https://app.test` com HMR funcionando, zero config**.
- `runtime:` monta o PATH do processo com a versão certa (`bin/node/22.x`), sem alterar o `current` global. Vários projetos Node com versões diferentes rodam ao mesmo tempo.
- Logs de cada process aparecem na aba do site. Start/stop/restart individual.
- `autostart: on-visit` (como o Herd 2.0): o process `web` só sobe quando alguém acessa o host; Nginx devolve página "Iniciando…" com auto-refresh via `error_page 502 = /_forgebox/starting`.

### 4.3 Node multi-versão
- `nodejs` é `layout: versioned` com `current_link` → `bin/node/current` no PATH (padrão do usuário hoje). `forge use node 20` troca a junction; nenhum env-var muda.
- Por projeto: `.nvmrc` / `.node-version` / `forgebox.yaml` → `forge shell` e processes respeitam. Corepack habilitado por padrão (pnpm/yarn). Os shims `node`/`npm`/`npx`/`corepack` já respeitam `.nvmrc`/`.node-version` (`install.version_files` do manifesto `nodejs`) e `spec.runtimes[]` do `forgebox.yaml` (§2.3); aliases como `lts/iron` não são entendidos (use a versão: `22` ou `v22.12.0`).
- Pacotes globais (`npm -g`) ficam em `data/node/<version>/npm-global` para não vazar entre versões. Nos processes isso vem do manifesto (`config.env` do `nodejs`: `npm_config_prefix` e `NODE_EXTRA_CA_CERTS`, §4.4); o `.npmrc` gerado para o terminal do usuário não existe ainda (a pasta `npm-global` não entra no PATH dos processes: `npm i -g` + comando global dentro de um process é limitação conhecida).

### 4.4 Implementação (M1-E4-T5)

Proxies: `forge_sites::proxy` (`ProxyOptions {websocket, headers, read_timeout_secs, start_on_visit}`, `ProxyTarget::parse`, `proxy_vhost`). O upstream é `http(s)://host[:porta]` sem caminho, query nem credenciais; WebSocket vem **ligado** (HMR); `websocket: false` de `spec.proxies[]` é respeitado (`ProjectPlan::proxy_options` por host → `VhostContext::proxy_options`). Site `kind: app` (`root` = pasta do projeto; exatamente um process com `proxy: true`): `vhost::build` aponta o host para `127.0.0.1:<porta>` desse process (`VhostContext::app_ports`, preenchido por `AppManager::allocate_ports`; porta fixa vem do próprio process; porta ainda não alocada → `conflict`) e liga `start_on_visit` quando ele é `autostart: on-visit`. `proxy_path` ainda não é renderizado (`not_implemented`). Um `forgebox.yaml` com `proxy: true` num process faz o site principal virar `app` (driver e PHP descartados). Decisões: [ADR-0028](adr/0028-app-processes-launch-path-on-visit.md).

Processes (`forge_sites::apps`, `AppManager`): `AppSite` (processes do site + pasta do projeto + `spec.runtimes[]`) → `AppPlan::build` (bloqueante: valida, resolve runtime/PATH/executável/log) → porta → `AppPlan::finish(porta)` → `AppSpec` → `InstanceSpec` do supervisor.
- **Comando.** `cmd` com `$PORT`/`${PORT}` substituído (também nos valores de `env`; usar `$PORT` sem porta é erro). Primeira palavra `.exe`/`.com` achada no PATH do process → lançamento direto; `.cmd`/`.bat` (`npm`, `npx`, `pnpm`), internos do `cmd.exe` ou `& | < > ^ %` fora de aspas → `cmd.exe /d /c %FORGEBOX_COMMAND%` com a linha na variável (aspas chegam intactas). Palavra não encontrada → `not_found` listando as pastas procuradas.
- **Porta.** `port: auto` (faixa `apps` 3000–3999, `PortReservations::allocate`, dono `{app, <site id>, <process>}`, idempotente: a mesma porta após reiniciar), `port: N` (`reserve`) ou nenhuma. Sem `port:`: `auto` se `proxy: true` ou o comando/env mencionam `$PORT`, senão nenhuma (filas e schedulers não gastam porta). Com porta, o process ganha health TCP em `127.0.0.1:<porta>` (`Running` = escutando; até 120 s) e a pré-checagem de porta.
- **Runtime.** `runtime: tool@seletor` (`nodejs@22` = a mais nova `22.x` instalada; sem correspondência → `not_found` listando o instalado). Sem `runtime:`: a ferramenta que fornece a primeira palavra (`install.shims` do manifesto: `npm` ⇒ `nodejs`), com a versão de `.nvmrc`/`.node-version`/`.php-version` (`install.version_files`; do `cwd` até a pasta do projeto, o mais próximo vence), senão `spec.runtimes[]` do `forgebox.yaml`, senão `current`, senão a mais nova.
- **PATH** = pastas do runtime (`install.path` sob `bin/<tool>/<versão>`) + `AppSite::extra_path` (dependências) + pasta do sistema; nunca o PATH do usuário. **Ambiente** = env mínimo do supervisor + `config.env` do runtime + `env` do site + `env` do process + `PORT` + `FORGEBOX_SITE`. Para o Node (manifesto): `NODE_EXTRA_CA_CERTS=<raiz>/certs/ca/forgebox-ca.crt` e `npm_config_prefix=<raiz>/data/node/<versão>/npm-global`.
- **Supervisão.** Um `InstanceActor` por process (tool `app`, instância `<host>.<process>`, eventos `service.*`), todos num Job aninhado por site; logs em `logs/apps/<host>/<process>.log`. `up` (planeja, aloca portas, para o que o site já rodava, registra e inicia os `autostart: true` esperando escutarem), `down`, `forget` (também libera as portas), `start_process`/`stop_process`/`restart_process` (por process ou todos), `status`, `wait_ready`. Process parado pelo usuário não é iniciado por visita. `restart: never|failure|always` mapeia para a política do supervisor (`failure` = 3 em 60 s).
- **`autostart: on-visit` (gatilho HTTP é do daemon, M1-E5).** O servidor interno do daemon atende `/_forgebox/starting?host=<host>` chamando `AppManager::starting_page(host, lang)`: ela chama `ensure_started(host)` (**só a primeira chamada por registro inicia** os processes on-visit, sem esperar; as seguintes devolvem `already`) e renderiza `pages/starting.html.j2` (503 + `Retry-After: 2` e refresh de 2 s; 500 sem refresh quando falhou; pt-BR ou en-US pelo `Accept-Language`). O refresh repete a URL original, que o nginx proxia assim que o app escuta. O teste `nginx_proxy_online` faz o papel do daemon com um listener de 30 linhas. No daemon (M1-E5-T2): `forge_daemon::internal`, em `127.0.0.1` na porta da faixa `internal` (47481 por padrão), que vai para `prefs.internal_port` dos templates; também responde `/_forgebox/health`.
- **Aceite medido** (esta máquina, Node 20.12.2, nginx 1.30.6 real): servidor Node com HTTP + upgrade WebSocket sobe como process `web` (`port: auto` → 3000, `PATH` = pasta do runtime + `C:\Windows\system32`, `FORGEBOX_SITE`, `NODE_EXTRA_CA_CERTS`); GET e eco WebSocket passam **pelo nginx** renderizado (`proxy_pass http://127.0.0.1:3000`, `Upgrade`/`Connection`), que é o que o HMR do Next.js/Vite usa; a primeira visita a um site on-visit recebe a página "Iniciando", sobe o process e a visita seguinte passa. O `create-next-app` real com TLS em `https://app.test` (DNS + CA do usuário) fica para o smoke M1.

## 5. DNS: `*.test` sem editar `hosts`

### 5.1 Estratégia (ordem de preferência)
1. **Resolver próprio + NRPT** (padrão): `forge-net::dns` sobe um servidor DNS (hickory) em `127.0.0.1:53` (ou `127.0.0.53:53`) respondendo `A 127.0.0.1` e `AAAA ::1` para as zonas configuradas (`.test` por padrão; `.local` é desencorajado por mDNS, `.localhost` já resolve nativamente). O helper elevado adiciona **uma** regra NRPT: `Add-DnsClientNrptRule -Namespace ".test" -NameServers 127.0.0.1` (equivalente via API `DnsClientNrptRule` WMI). A partir daí, qualquer `*.test`, inclusive subdomínios arbitrários, resolve sem tocar em `hosts`. Funciona em Windows 10/11 em todas as edições. Sem admin → cai para o modo 2.
2. **hosts gerenciado** (fallback): bloco `# forgebox:begin … end` em `C:\Windows\System32\drivers\etc\hosts` com uma linha por host **e por alias** (sem wildcard). Escrita via helper. Flush de cache DNS após alteração.
3. **Sem gerenciamento**: usuário cuida; a UI mostra diagnóstico "loja.test não resolve para 127.0.0.1".

### 5.2 Diagnóstico sempre visível
Ao criar/abrir um site a UI resolve o host via API do Windows e via nosso resolver, comparando. Problemas comuns detectados: VPN que sobrescreve DNS, Docker Desktop com `.test`, Acrylic DNS instalado, IPv6 `::1` ausente (adicionamos `AAAA ::1`).

### 5.3 Acesso pela LAN/celular
`forge site expose-lan loja.test` → opcional: servidor escuta também em `0.0.0.0`, regra de firewall via helper, QR code com `https://<ip-da-maquina>` + instrução para instalar a CA no celular (`http://<ip>/_forgebox/ca.crt`). Para domínio real sem CA: integração `*.local-ip.sh`/`nip.io` como alias (Laragon usa `192-168-137-1.local-ip.sh`).

### 5.4 Implementação (M1-E4-T3)
- **Liga/desliga** (M1-E5-T2): `[dns] enabled` no `forgebox.toml` (padrão `true`) faz o daemon subir o resolvedor no boot ([01 §5.1](01-architecture.md) passo 5; porta ocupada vira aviso, não falha de boot); `net.dns_enable`/`net.dns_disable` gravam a chave, sobem/param o resolvedor e aplicam/removem a política do SO.
- **Zonas** (`forge_net::dns::ZoneSet`): uma zona local por TLD (`tld` + `[dns] extra_tlds` do `forgebox.toml`); **qualquer** nome da zona, inclusive o ápice, responde `A 127.0.0.1` e `AAAA ::1`, TTL 30 s. Zonas remotas (`[[dns.remote_zones]] zone = "hml.test", address = "10.0.0.5"`, [13](13-remote-mode.md) §5) ficam abaixo de um TLD e vencem por serem mais específicas; família sem endereço responde NOERROR vazio (nunca NXDOMAIN). Outros tipos (`MX`, `TXT`, `HTTPS`...) respondem NOERROR vazio com o `SOA` da zona (TTL negativo 10 s). Fora das zonas: NXDOMAIN, sem `aa`, **sem repasse** a upstream (a NRPT só manda as zonas para cá). Classe diferente de `IN` → REFUSED.
- **Servidor** (`forge_net::dns::DnsServer`): hickory-server UDP + TCP no mesmo endereço, ator `dns-server` ([03](03-engine.md) §7.2) com `start` (primeiro endereço livre de `127.0.0.1:53`, `127.0.0.53:53`; nenhum livre → `port_in_use` com os motivos), `stop`, `reload` (troca as zonas na hora via `watch`, sem mexer nos sockets), `stats` (endereço, `nrpt_capable` = porta 53 em loopback, contadores de perguntas/respostas/NODATA/NXDOMAIN/erros, último erro). Zonas, contadores e "deveria estar rodando" ficam fora do ator: reiniciado após pânico, ele volta a escutar sozinho. Medido nesta máquina: o Host Network Service (Hyper-V/WSL, `svchost`) ocupa `0.0.0.0:53` UDP e mesmo assim `127.0.0.1:53` é aceito (o bind mais específico vence).
- **Política** (`forge_net::policy::DnsPolicyManager` sobre `forge_core::platform::DnsPolicy`): `enable` tenta NRPT para cada TLD (`nrpt.add` via helper, [ADR-0022](adr/0022-elevated-helper-protocol.md); regra já correta não pede UAC) e confere a leitura depois; sem porta 53, sem NRPT no SO ou com regras NRPT de **política de grupo** (enquanto existirem, o Windows ignora as locais) cai para `hosts.set_block` (`127.0.0.1` e `::1` por host e alias); UAC recusado → `needs_elevation` sem tentar mais nada. `disable` remove a regra Forgebox e o bloco; `sync_hosts` reescreve o bloco só quando os sites mudam. Toda mudança termina com `DnsFlushResolverCache` e publica `dns.policy_applied {v, tld, policy}` e `helper.op {v, name, ok, error}`. Leitura sem elevação: regras NRPT do registro (`HKLM\SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig` e `HKLM\SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig`), `hosts` em `DataBasePath` de `Tcpip\Parameters`.
- **Escrita pelo helper** (M1-E5-T4, [ADR-0024](adr/0024-helper-execution.md)): `nrpt.add`/`nrpt.remove` chamam por WMI os métodos estáticos `Add`/`Remove` de `PS_DnsClientNrptRule` (`root/Microsoft/Windows/DNS`, a classe por trás de `Add-DnsClientNrptRule`), sem PowerShell; a regra leva `Comment = "Forgebox"` e só regras locais com esse comentário e namespace exato são substituídas ou removidas (`nrpt.add` com a regra já correta não muda nada). Com regras de política de grupo o helper grava só a regra local e responde `effective: false`; o daemon segue para o `hosts`. `hosts.set_block` troca o arquivo de forma atômica (`ReplaceFileW`, mantém ACL e atributos) e preserva byte a byte o que está fora do bloco, em qualquer codificação; sem mudança, não escreve.
- **Bloco no `hosts`** (`forge_core::helper::hosts`): mesmos marcadores e hash dos blocos gerenciados do `forge-render` (`# forgebox:begin hosts (hash=…)`), mantém as quebras de linha do arquivo (CRLF no `hosts` de fábrica), fora do bloco nada muda byte a byte.
- **Diagnóstico** (`forge_net::dns::DnsDiagnostics::diagnose(host, zonas, servidor)` → `DnsDiagnosis`): resolve pelo SO (`getaddrinfo`: hosts, NRPT, cache) e direto no nosso resolvedor (UDP), lê regras NRPT, `hosts`, donos da porta 53 (UDP e TCP, `PortTable::udp_endpoints`) e adaptadores (`NetAdapters`), tudo em paralelo e com timeout; devolve `system_resolves_to`, `our_server_resolves_to`, `nrpt_present`, `hosts_block_present`, `hosts_entries`, `port53_owner`, `vpn_adapters`, `ok` e `hints` com código estável, severidade e texto pt-BR (`server_not_running`, `server_not_on_port53`, `nrpt_missing`, `nrpt_points_elsewhere`, `group_policy_nrpt`, `hosts_override`, `port53_acrylic`, `port53_docker`, `port53_dns_server`, `port53_vpn`, `port53_system_service`, `vpn_adapter`, `system_mismatch`, `system_unresolved`...). Fonte ilegível vai para `read_errors`; o diagnóstico nunca falha.

### 5.5 Teste manual (aceite de M1-E4-T3, smoke M1)
Pedem um humano diante da máquina (o prompt UAC não é automatizável). Com o daemon e a CLI (M1-E5-T2/T3):
```powershell
forge dns enable                       # 1 prompt UAC; cria a regra NRPT .test -> 127.0.0.1 (ou 127.0.0.53)
Get-DnsClientNrptRule | Where-Object Namespace -contains '.test'
Resolve-DnsName qualquer.test          # esperado: A 127.0.0.1 e AAAA ::1
Resolve-DnsName api.qualquer.test      # subdomínio arbitrário, idem
Resolve-DnsName google.com             # continua no DNS normal
forge dns test qualquer.test           # diagnóstico: ok = true, sem hints de erro
forge dns disable                      # 1 prompt; Resolve-DnsName qualquer.test volta a falhar
```
Antes da CLI existir, o mesmo pode ser feito com o exemplo do crate e a regra criada à mão num PowerShell elevado:
```powershell
cargo run -p forge-net --example dns_serve -- --diagnose qualquer.test --for 300   # sem admin
Resolve-DnsName qualquer.test -Server 127.0.0.1 -DnsOnly                           # direto no resolvedor
# PowerShell como administrador (equivale ao nrpt.add do helper):
Add-DnsClientNrptRule -Namespace ".test" -NameServers "127.0.0.1" -Comment "Forgebox (teste manual)"
Resolve-DnsName qualquer.test                                                      # via NRPT
Get-DnsClientNrptRule | Where-Object Comment -like 'Forgebox*' | Remove-DnsClientNrptRule -Force
```
Executado nesta máquina (Windows 10 19045, sem elevação) com o exemplo em `127.0.0.1:53`: `Resolve-DnsName qualquer.test -Server 127.0.0.1` → `A 127.0.0.1` + `AAAA ::1` (TTL 30); `loja.hml.test` com `--remote hml.test=10.0.0.5` → `A 10.0.0.5`; `-TcpOnly` idem; `x.com` → "O nome DNS não existe". A parte com NRPT (`Add-DnsClientNrptRule`) fica para o smoke M1.

## 6. SSL/TLS

### 6.1 CA local
- Gerada no onboarding (ou no primeiro `tls.issue`): ECDSA P-256, `CN=Forgebox Local CA <hostname> <ano>`, `O=Forgebox`, `OU=created <instante>`, validade 10 anos, `pathLenConstraint = 0`, em `certs/ca/forgebox-ca.crt|key` (chave com ACL só do usuário, aplicada antes de o segredo ser escrito). Implementação: `forge_tls::ca` ([ADR-0023](adr/0023-local-ca-name-constraints-trust.md)).
- **Name constraints** (críticas, `permittedSubtrees`): `dNSName` de cada TLD configurado que seja de uso local (`.test`, `[dns] extra_tlds` como `.localhost`; TLD público como `.dev` fica de fora), `localhost` sempre, `iPAddress` `127.0.0.0/8` e `::1/128` ([11 §3](11-security.md)). TLD acrescentado depois só entra com rotação (`forge tls ca status` avisa).
- Instalada em `CurrentUser\Root` via `CertAddEncodedCertificateToStore` (**não requer admin**; Windows mostra um diálogo de confirmação uma única vez; "Não" = `cancelled`). Opcional via helper: `LocalMachine\Root` para todos os usuários (`trust.add_machine_root {thumbprint}`). Raízes identificadas pelo thumbprint SHA-1 (o que `certmgr`/`Cert:\` mostram) e registradas em `certs/ca/trust.json` para remoção limpa. Modo portátil: nunca toca no store.
- Firefox: 120+ já lê as raízes do usuário do Windows; para versões antigas instruímos `policies.json` (`Certificates.ImportEnterpriseRoots`). Node: definimos `NODE_EXTRA_CA_CERTS` nos processes gerenciados e sugerimos no PATH setup. Java/Python: botão "Exportar CA" e instruções (`forge_tls::trust::instructions`; nada disso é automático).
- Renovação/rotação: 90 dias antes de expirar a UI avisa; rotação re-emite todas as folhas. A rotação é em duas fases: a nova CA é confiada onde a antiga era **antes** de qualquer arquivo mudar (recusar o diálogo não muda nada), depois a CA é gravada, as folhas reemitidas e a antiga removida (recusa vira aviso; o thumbprint fica no `trust.json`).

### 6.2 Certificados de site
- Por site: SAN = host + `*.host` + aliases; `CN` = host; EKU `serverAuth`; validade 825 dias (limite Apple/Chrome, contados do `notBefore` recuado 1 h, nunca além da CA); ECDSA P-256; arquivos `certs/sites/<host>.crt|key` (só a folha no `.crt`; chave com ACL só do usuário). O host precisa estar abaixo de um TLD da CA (nunca um certificado para `*.test`). Implementação: `forge_tls::leaf`.
- Emitidos no `secure: true` (`tls.issue` devolve o existente se ainda cobre os nomes), renovados automaticamente 30 dias antes pelo ator `tls` (verificação diária, ±10 %; também reemite folhas de outra CA, com chave que não bate ou com ponto de distribuição de CRL diferente do configurado), re-render + reload (o daemon reage a `tls.renewed`).
- **Revogação para o Schannel:** cada folha leva o ponto de distribuição de CRL `http://127.0.0.1:47480/forgebox/<SHA-1 da CA>.crl` (faixa `tls-crl` do `forgebox.toml`) e o ator `tls` serve ali uma CRL vazia assinada pela CA (cópia em `certs/ca/forgebox-ca.crl`, 7 dias, reassinada na metade). Sem isso o `curl.exe` do Windows recusa o certificado por padrão (`CRYPT_E_NO_REVOCATION_CHECK`), como acontece com mkcert/Laragon ([ADR-0023](adr/0023-local-ca-name-constraints-trust.md)).
- Export para `.pfx` para IIS/Outros (pendente: `tls.export` com `pfx`; PEM já sai por `TlsManager::export_leaf`).

### 6.3 Certificados públicos (opcional, v1.1)
ACME (Let's Encrypt via DNS-01 Cloudflare) para quem expõe via túnel com domínio próprio.

### 6.4 Verificação manual (aceite do smoke M1)
Com nginx servindo `loja.test`, DNS ligado, daemon rodando (responder de CRL em `127.0.0.1:47480`) e a CA instalada (`forge tls ca install`):
```
C:\Windows\System32\curl.exe https://loja.test   # sem -k e sem --ssl-no-revoke: Schannel valida pela CA em CurrentUser\Root e busca a CRL
openssl x509 -in C:\Forgebox\certs\ca\forgebox-ca.crt -noout -text
#   X509v3 Basic Constraints: critical  CA:TRUE, pathlen:0
#   X509v3 Name Constraints: critical
#       Permitted:
#         DNS:test
#         DNS:localhost
#         IP:127.0.0.0/255.0.0.0
#         IP:0:0:0:0:0:0:0:1/FFFF:FFFF:FFFF:FFFF:FFFF:FFFF:FFFF:FFFF
openssl verify -CAfile C:\Forgebox\certs\ca\forgebox-ca.crt -verify_hostname loja.test C:\Forgebox\certs\sites\loja.test.crt
certutil -user -verifystore Root <thumbprint SHA-1>   # a CA está no store do usuário
curl -s -o NUL -w "%{http_code}\n" http://127.0.0.1:47480/forgebox/<thumbprint SHA-1>.crl   # 200
```
Sem daemon nem nginx: `cargo run -p forge-tls --example tls_demo -- <raiz> issue loja.test` (CA + folha + verificação pelo CryptoAPI com revogação) e `... serve loja.test 18443` (HTTPS de teste + responder de CRL), depois `curl.exe --cacert <raiz>\certs\ca\forgebox-ca.crt --resolve loja.test:18443:127.0.0.1 https://loja.test:18443/`.
Sem nginx, a mesma garantia é coberta por testes: `crates/forge-tls/tests/name_constraints_tls.rs` (handshake rustls/webpki) e `windows_trust.rs` (CryptoAPI com a CA como raiz exclusiva: `loja.test` passa, `google.com`/`evil.example.com` assinados com a chave vazada falham com `CERT_TRUST_HAS_NOT_PERMITTED_NAME_CONSTRAINT`).

### 6.5 Bundle de CAs para ferramentas com TLS próprio
O OpenSSL e o curl do PHP (e o Composer por cima deles) não leem o repositório do Windows: só confiam num arquivo de CAs. Sem ele, numa rede com inspeção TLS corporativa (raiz da empresa distribuída por GPO), `composer create-project` falha com `curl error 60` e um app que chama `https://api.loja.test` não confia na CA local. O ator `tls` grava `certs/ca-bundle.pem` ([ADR-0032](adr/0032-m1-gate-integration.md), `forge_tls::bundle`): a CA local (quando existe) + toda raiz que o Windows confia agora (`TrustStore::system_roots`: o store lógico `Root` do usuário — raízes do usuário, da máquina, de GPO e enterprise — e as raízes de terceiros `LocalMachine\AuthRoot`; só certificados no prazo, sem duplicatas), lidas sem elevação nem diálogo. Ficam de fora o que o Windows recusa mesmo estando nesses stores: certificados do store `Disallowed` (usuário + máquina) e raízes cujas "finalidades habilitadas" (propriedade EKU por certificado, `CERT_ENHKEY_USAGE_PROP_ID`, definida pelo admin ou pela atualização automática de raízes) não incluem autenticação de servidor (`1.3.6.1.5.5.7.3.1`) — o OpenSSL não conhece nenhum dos dois. Restrições que o arquivo PEM não expressa (`CERT_DISALLOWED_ENHKEY_USAGE_PROP_ID`, datas `NotBefore` de distrust, CTL de raízes não confiáveis baixada pelo Windows Update) não são aplicadas. Reescrito no start do ator e quando a CA local é criada ou rotacionada (só se o conteúdo mudou). O overlay do PHP aponta `openssl.cafile` e `curl.cainfo` para ele, então o PHP confia no mesmo que o navegador. São só certificados públicos (sem chave). Limite: raízes que o Windows ainda não baixou (atualização automática de raízes) só entram depois de o Windows usá-las uma vez; o arquivo é regenerado no próximo start do daemon.

## 7. Túneis (`forge share`)
- Tools `cloudflared`, `ngrok`, `expose` no catálogo (`category: tunnel`).
- `forge share loja.test [--provider cloudflared]`: sobe túnel apontando para `https://loja.test` local (ou `http://127.0.0.1:80` com `Host` header), mostra URL pública + QR na UI, loga requests. cloudflared "quick tunnel" não exige conta; com conta, túneis nomeados e domínio próprio.

## 8. Páginas internas (`/_forgebox/*`)
Servidas por um site interno `forgebox.test` (e como fallback de `localhost`): dashboard web mínima, `phpinfo` por versão, `health.php` dos pools, `ca.crt`, página "site iniciando", lista de sites (substitui o `index.php` do Laragon em `etc/apps/laragon`).

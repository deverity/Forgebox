# 13 — Modo remoto (controlar um Forgebox em outra máquina)

## 1. Objetivo
Instalar o motor (`forgeboxd` + `forge`) em um **Linux** (VPS, servidor de homologação, NAS, VM, WSL) ou em outro Windows, e **controlar tudo a partir da GUI/CLI no Windows do desenvolvedor** como se fosse local: ver serviços, criar sites, trocar PHP, ler logs, ver métricas. Também o inverso (GUI no macOS/Linux controlando um Windows).

Isso é viável porque, desde a v1, GUI e CLI já são **clientes finos** da API JSON-RPC do daemon ([01](01-architecture.md) §1). O modo remoto é essencialmente: *o mesmo cliente, outro transporte, com autenticação*.

## 2. O que precisa estar pronto na v1 para não doer depois (decisões antecipadas)

| Decisão | Por quê agora |
|---|---|
| `forge-ipc` abstrai o transporte (`Transport` trait: `NamedPipe`, `UnixSocket`, `WebSocketTls`) | Adicionar transporte depois é barato; mudar a forma dos contratos não |
| Contratos JSON-RPC **nunca** carregam caminhos absolutos do servidor como identidade. IDs são ULIDs; caminhos são atributos informativos | A GUI remota não pode assumir `C:\Forgebox` |
| `ForgeRoot` em `forge-core::paths` já é abstrato (traits `platform::*`) | Impl Linux: `~/.local/share/forgebox` ou `/opt/forgebox`, symlinks em vez de junctions, systemd-user em vez de autostart |
| Eventos e métricas são **streams assináveis com filtro e backpressure** | Rede tem latência e pode cair; o cliente precisa reassinar e pedir "desde o seq N" |
| Toda operação é idempotente e tem `request_id` | Retentativas em rede não podem criar dois sites |
| Transferência de arquivos é um método da API (`fs.read/write/tail` restrito a `etc/`, `logs/`, `www/`) | A GUI remota edita `php.ini` e lê logs sem SMB/SSH |
| Download de tools é feito **pelo daemon remoto**, nunca pelo cliente | O servidor baixa de windows.php.net/apt/github diretamente |
| Ações que abrem coisas no SO (`open in editor`, `browse`) são marcadas `local_only` ou traduzidas (abrir `https://loja.test` do remoto = abrir `https://loja.<remote>.test` ou túnel) | Evita botões quebrados na GUI remota |

Implementado em M1-E5-T1 ([ADR-0015](adr/0015-ipc-protocol.md)): `forge_ipc::transport::Transport` (named pipe, unix socket mínimo, em memória; `WebSocketTls` é placeholder até M6), `request_id` obrigatório em todo request com cache de idempotência no servidor, papéis e `local_only` na tabela `METHODS` (conexões não locais recebem `forbidden` nos métodos `local_only`), streams `events|logs|metrics.subscribe` com `seq`, fila drop-oldest + `*.lagged` e `resume_from`, e `fs.read|write|tail|list` restritos a `etc/`, `logs/`, `www/`. O cliente reconecta e reassina sozinho a partir do último `seq` recebido (ou, se nada chegou ainda, do `last_seq` devolvido pelo subscribe), sem perder nem repetir eventos.

## 3. Arquitetura do modo remoto

```
Windows (cliente)                                   Linux (remoto)
┌───────────────────────┐        WSS :7788 (mTLS)     ┌────────────────────────────┐
│ Forgebox.exe / forge  │ ─────────────────────────▶  │ forgeboxd --listen 0.0.0.0 │
│  ┌ perfis de conexão  │   JSON-RPC + event stream   │  • mesma API                │
│  │ local (pipe)       │ ◀─────────────────────────  │  • providers Linux          │
│  │ hml (wss://...)    │                             │  • systemd-user, symlinks   │
│  │ ssh:vps (túnel)    │                             │  • php-fpm nativo OU pools  │
└──┴────────────────────┘                             └────────────────────────────┘
```

### 3.1 Transportes
1. **Local**: named pipe (Windows) / unix socket (Linux, macOS). Padrão.
2. **WSS direto** (`wss://host:7788`): o daemon remoto escuta com TLS. Certificado: emitido pela **CA do próprio daemon remoto** (já existe, [06](06-sites-vhosts-multi-php.md) §6) ou por ACME se tiver domínio. Autenticação **mTLS**: o cliente apresenta um certificado emitido no "pareamento" (§4). Alternativa mais simples: token Bearer de longa duração + TLS do servidor (para quem não quer mTLS).
3. **Via SSH** (`ssh://user@host`): `forge` abre túnel SSH (crate `russh`) até o unix socket remoto. Zero portas abertas, reaproveita chaves SSH existentes. Ideal para VPS.
4. **Via túnel de saída** (futuro): o remoto se conecta a um relay (cloudflared/Tailscale/relay próprio) e o cliente entra pelo relay. Para máquinas atrás de NAT.

### 3.2 Pareamento (WSS/mTLS)
```
no remoto:   forge remote pair            → imprime código de 8 dígitos + fingerprint da CA (válido 10 min)
no cliente:  forge remote add hml wss://10.0.0.5:7788 --code 1234-5678
             → troca de chaves (SPAKE2 com o código), cliente recebe cert mTLS assinado pela CA remota
             → perfil salvo em state/remotes/hml.toml (cert, chave com ACL, fingerprint pinado)
```
Revogação no remoto: `forge remote clients` / `forge remote revoke <id>`.

### 3.3 Autorização
Perfis por cliente: `admin` (tudo), `operator` (start/stop/logs/sites, sem instalar/remover tools nem ler segredos), `viewer` (somente leitura + métricas). Guardado no remoto; aplicado por método JSON-RPC.

## 4. Diferenças por SO que o daemon abstrai

| Capacidade | Windows | Linux |
|---|---|---|
| Supervisão de filhos | Job Objects | cgroup v2 (user slice) + `prctl(PR_SET_PDEATHSIG)`; opcional registrar como unidade systemd-user transitória |
| Versão ativa CLI | junction `current` | symlink `current` |
| PHP web | pools `php-cgi` próprios | **php-fpm** nativo por versão (pool por versão com `listen` em porta/socket) **ou** pools próprios com `php-cgi` para paridade; decisão por manifesto (`service.pool.backend: fpm | cgi`) |
| Providers de PHP | windows.php.net | binários estáticos (ex. `static-php-cli`), ou pacotes do distro (`apt` via provider `apt` com sury/remi), ou build. Catálogo marca `platforms` por release |
| DNS `*.test` | resolver + NRPT | resolver + `systemd-resolved` (`~test` routing domain) ou `/etc/resolver`-like; no remoto normalmente o cliente é quem precisa resolver `*.hml.test` → IP do remoto |
| CA | CurrentUser\Root | `update-ca-certificates`/`trust anchor` (requer sudo → helper `pkexec`) |
| Portas < 1024 | livre | `CAP_NET_BIND_SERVICE` via `setcap` no binário do servidor web ou `authbind` (helper) |
| Autostart | HKCU Run | systemd-user `forgeboxd.service` + `loginctl enable-linger` |
| Helper elevado | UAC | `pkexec`/polkit policy ou `sudo` com allowlist |

## 5. Experiência na GUI
- Seletor de **contexto** no topo da sidebar: "Local · hml (Linux) · vps". Troca instantânea; cada contexto mantém seu estado/cache. Cor de destaque diferente por contexto remoto (evita start/stop no servidor errado).
- Indicador de latência e de conexão; reconexão automática com replay de eventos desde o último `seq`.
- Ações `local_only` aparecem com explicação ("Abrir no editor não está disponível em remotos; use 'Abrir via SSH no VS Code'", que abre `vscode-remote://ssh-remote+host/path`).
- DNS do lado do cliente: ao adicionar um remoto, oferecer zona `*.hml.test → 10.0.0.5` no resolver local do Windows (já existe), para que `https://loja.hml.test` abra no navegador do desenvolvedor com a CA remota instalada (botão "Confiar na CA do remoto"). A zona fica em `forgebox.toml` (`[[dns.remote_zones]] zone = "hml.test", address = "10.0.0.5"`), sempre abaixo de um TLD local, e é respondida pelo resolver local (`forge_net::dns::ZoneSet`, mais específica vence; sem `AAAA` quando o remoto só tem IPv4); a regra NRPT do TLD já a cobre ([06](06-sites-vhosts-multi-php.md) §5.4).
- Métricas ([14](14-observability-metrics.md)) funcionam igual, vindas do remoto.

## 6. Headless em servidor
- `forge-1.0.0-linux-x64.tar.gz` com `forgeboxd` e `forge`; `forge daemon install --user` cria unidade systemd-user; `forge remote listen --addr 0.0.0.0:7788 --tls auto`.
- Páginas internas `/_forgebox/` servem um painel web mínimo (somente leitura + login) como fallback quando não há GUI.

## 7. Fora de escopo (por enquanto)
- Sincronizar sites/arquivos entre máquinas (isso é git/rsync).
- Multi-tenant com vários usuários Linux donos de serviços diferentes (o daemon remoto é de **um** usuário do SO).

## 8. Roadmap
- **v1 (Windows)**: tudo da seção 2 implementado (transporte abstrato, IDs, streams com `seq`, `fs.*` restrito, `local_only`). Custo pequeno, benefício grande.
- **v1.x**: build Linux headless do daemon + CLI; transporte SSH; GUI com seletor de contexto.
- **v2**: WSS/mTLS com pareamento, perfis de autorização, relay.

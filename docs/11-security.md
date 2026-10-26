# 11 — Segurança

Um ambiente de desenvolvimento local tem privilégios perigosos: edita `hosts`, instala uma CA raiz, baixa e executa binários, expõe portas. O Forgebox trata isso explicitamente.

## 1. Modelo de privilégios
- **Daemon, GUI e CLI rodam como o usuário**, sem elevação. Ligar nas portas 80/443 no Windows não exige admin.
- **`forgebox-helper.exe`** é o único binário elevado (manifest `requireAdministrator`). Executa **apenas** operações de uma allowlist fechada, cada uma com parâmetros validados:
  | op | validação |
  |---|---|
  | `nrpt.add {namespace}` / `nrpt.remove` | namespace ∈ TLDs configurados (`.test`, …), nameserver fixo `127.0.0.1` |
  | `hosts.set_block {entries}` | só hosts `[a-z0-9.-]+` que terminem em TLD configurado; só IPs `127.0.0.1`/`::1`; escreve só dentro do bloco gerenciado |
  | `trust.add_machine_root {thumbprint}` / `remove` | cert deve estar em `certs/ca/` e bater com o thumbprint |
  | `firewall.add {name, port, exe}` / `remove` | exe dentro de `<root>/bin/`, nome prefixado `Forgebox-` |
  | `defender.exclude {path}` / `remove` | path = raiz do Forgebox |
  | `vcredist.install {path, sha256}` | arquivo baixado pelo daemon, hash conferido, Authenticode Microsoft verificado |
- O helper é invocado por processo (ShellExecute `runas`, um UAC por operação ou por lote agrupado no onboarding). Modo opcional "serviço helper" (como HerdHelper) para quem não quer UAC: serviço Windows ouvindo em named pipe com ACL restrita ao SID do usuário instalador, mesma allowlist. Padrão: **sem serviço**.
- Tudo que o helper faz vai para `logs/helper.log` e para o histórico de eventos.

## 2. Cadeia de confiança dos downloads
1. Registry oficial assinado (minisign) e verificado antes de qualquer manifesto ser lido. Registries de terceiros exibem aviso e não podem declarar `post_install.run`.
2. Cada download: SHA-256 obrigatório (do upstream ou curado no registry pelo bot, com PR revisado). Sem hash → bloqueado por padrão.
3. Assinaturas upstream verificadas quando existem (PGP nginx/Node, Authenticode).
4. Staging em `tmp/stage` e extração com proteção contra zip-slip (`..`, caminhos absolutos, symlinks dentro do zip rejeitados).
5. `post_install` só executa binários dentro da própria versão instalada.

## 3. CA local
- Chave privada da CA em `certs/ca/forgebox-ca.key` com ACL só do usuário; nunca sai da máquina; nunca é lida pela GUI (só o daemon).
- Instalação em `CurrentUser\Root` (não afeta outros usuários) por padrão. `LocalMachine` só via helper e opt-in explícito.
- Name constraints no certificado da CA (`permittedSubtrees: .test, .localhost, IPs 127.0.0.0/8`) para que, mesmo vazando a chave, ela não possa assinar `google.com`. Isso é algo que mkcert e Herd **não** fazem.
- Rotação em 1 clique; thumbprint salvo para remoção limpa na desinstalação.

## 4. API local (IPC)
- Named pipe `\\.\pipe\forgebox` (raiz padrão; outras raízes usam `\\.\pipe\forgebox-<hash da raiz>`) com descritor `O:<SID>D:P(A;;GA;;;<SID>)`: dono e único acesso = usuário atual, `PIPE_REJECT_REMOTE_CLIENTS` e primeira instância exclusiva (`FILE_FLAG_FIRST_PIPE_INSTANCE`: um segundo daemon ou um posseiro não compartilha o nome). O cliente confere que o pipe pertence ao usuário atual antes de enviar qualquer coisa (`forge_core::platform::windows::pipe_security`).
- Toda conexão começa com `system.hello {token, client, api_version}`; o token (256 bits, `state/ipc.token`) é criado pelo daemon: arquivo vazio → DACL protegida só do usuário (`FileAcl`) → segredo escrito → rename. No pipe é defesa em profundidade; token errado fecha a conexão. Conexão local = papel `admin`. Detalhes em [ADR-0015](adr/0015-ipc-protocol.md).
- WebSocket (`127.0.0.1:<porta>`) desligado por padrão; quando ligado exige o mesmo token em header. Sem CORS para origens não-locais; a GUI Tauri usa o pipe.
- `fs.*` da API só alcança `etc/`, `logs/` e `www/` (validação de sintaxe + prefixo canônico; links que escapam são recusados com `forbidden`).
- MCP server (`forge mcp serve`) via stdio apenas; ferramentas destrutivas (`remove`, `drop`) exigem `confirm: true` explícito no payload.

## 5. Serviços expostos
- Todos os serviços escutam em `127.0.0.1` por padrão (Laragon/XAMPP frequentemente usam `0.0.0.0`). Expor na LAN é ação explícita por serviço/site, com regra de firewall nomeada e aviso.
- MySQL/MariaDB: `root` sem senha **só em 127.0.0.1** (prática de dev), mas o Forgebox gera um usuário `forgebox` com senha aleatória para uso das próprias ferramentas e oferece "definir senha de root" no onboarding.
- PostgreSQL: `trust` apenas para `127.0.0.1/32`.
- Mailpit/MinIO/Meilisearch: credenciais padrão geradas aleatoriamente e exibidas/copiáveis (não `minioadmin/minioadmin`).

## 6. Segredos na UI
- `.env` viewer mascara chaves que casem `(KEY|SECRET|PASSWORD|TOKEN)`; "revelar" é por item e temporário.
- Logs de processes passam por redator básico para `Authorization:`/`password=`.

## 7. Supply chain do próprio Forgebox
- `cargo-deny` (licenças, advisories), `cargo-audit` em CI, lockfile commitado, builds reprodutíveis em GitHub Actions com SLSA provenance, binários assinados com certificado Authenticode do projeto (quando disponível) + minisign sempre.
- Telemetria: **desligada por padrão**; opt-in; só contadores agregados (versão, SO, features usadas); nunca hosts, caminhos ou IPs. Documentado na UI com o payload exato.

## 8. Ameaças consideradas e mitigação
| Ameaça | Mitigação |
|---|---|
| Registry comprometido injeta URL maliciosa | Assinatura minisign do index; hash por manifesto; PR review + bot |
| Zip malicioso com path traversal | Extrator rejeita; staging isolado |
| Processo local de outro usuário fala com o daemon | DACL do pipe |
| Site local malicioso usa a CA para MITM | Name constraints na CA |
| Pool PHP acessível por outras máquinas | Bind 127.0.0.1 em tudo; firewall só explícito |
| Helper elevado usado como escada de privilégio | Allowlist fechada, validação estrita, sem shell, log |
| Update do app substituído | Updater exige assinatura minisign; canal HTTPS |

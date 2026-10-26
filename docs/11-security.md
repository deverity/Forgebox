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
- **`forgebox-ctrl.exe`** é um auxiliar interno do daemon, **sem** elevação e sem rede: só se anexa ao console de um serviço do próprio usuário e gera `CTRL_BREAK` para o grupo dele (`forgebox-ctrl break <pid>`; [ADR-0015](adr/0015-ctrl-break-helper-process.md)).
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
- Named pipe `\\.\pipe\forgebox` com DACL = usuário atual. WebSocket (`127.0.0.1:<porta>`) desligado por padrão; quando ligado exige token de `state/ipc.token` (ACL do usuário) em header. Sem CORS para origens não-locais; a GUI Tauri usa o pipe.
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

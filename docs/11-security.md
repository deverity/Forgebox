# 11 — Segurança

Um ambiente de desenvolvimento local tem privilégios perigosos: edita `hosts`, instala uma CA raiz, baixa e executa binários, expõe portas. O Forgebox trata isso explicitamente.

## 1. Modelo de privilégios
- **Daemon, GUI e CLI rodam como o usuário**, sem elevação. Ligar nas portas 80/443 no Windows não exige admin.
- **`forgebox-helper.exe`** é o único binário elevado (manifest `requireAdministrator`). Executa **apenas** operações de uma allowlist fechada, cada uma com parâmetros validados:
  | op | validação |
  |---|---|
  | `nrpt.add {namespace, nameserver}` / `nrpt.remove {namespace}` | namespace (`.test`) ∈ TLDs configurados **e** sob um sufixo reservado a uso local (`test`, `localhost`, `invalid`, `example`, `local`, `internal`, `home.arpa`, `lan`, `home`, `corp`); nameserver ∈ {`127.0.0.1`, `127.0.0.53`} (onde o resolvedor conseguiu a porta 53, [ADR-0004](adr/0004-dns-nrpt-resolver.md)); `nrpt.add` substitui a regra Forgebox do mesmo namespace; `nrpt.remove` só remove regras do Forgebox |
  | `hosts.set_block {entries: [{ip, host}]}` | hosts `[a-z0-9.-]` (rótulos válidos, minúsculos) abaixo de um TLD configurado e de uso local; só IPs `127.0.0.1`/`::1`; sem duplicatas; no máximo 4096 linhas; escreve só dentro do bloco gerenciado `# forgebox:begin hosts`; lista vazia remove o bloco |
  | `trust.add_machine_root {thumbprint}` / `trust.remove_machine_root {thumbprint}` | thumbprint SHA-1 (40 hex); no add, o cert deve estar em `certs/ca/` (`.crt`/`.pem`/`.cer`, PEM ou DER), bater com o thumbprint **e** ser a CA do Forgebox (§3): autoassinada, `cA = true`, CN começando com `Forgebox`, Name Constraints com `dNSName` só sob sufixos de uso local e IPs só loopback/privados ([ADR-0024](adr/0024-helper-execution.md)); no remove, só certificados com CN começando com `Forgebox` |
  | `firewall.add {name, port, exe}` / `firewall.remove {name}` | nome `Forgebox-[A-Za-z0-9._-]{1,64}`; porta 1–65535 (TCP de entrada); exe `.exe` existente dentro de `<root>/bin/` (também depois de resolver junctions); a regra vale só nos perfis privado e domínio, grupo `Forgebox`, e substitui as de mesmo nome |
  | `defender.exclude {path}` / `defender.remove {path}` | path = raiz do Forgebox |
  | `vcredist.install {path, sha256}` | `.exe` dentro de `<root>/tmp/downloads/`, baixado pelo daemon (link permanente `https://aka.ms/vc14/vc_redist.x64.exe`, oferecido pela pré-checagem `missing_runtime`, [03](03-engine.md) §1.5; o conteúdo muda a cada release, então o sha256 é o do arquivo baixado, fixado pelo daemon), hash conferido, Authenticode Microsoft verificado — na **cópia** que o helper faz para `%SystemRoot%\Temp\forgebox-helper-<request_id>` (DACL `SYSTEM` + `Administrators`), que é a que roda (`/install /quiet /norestart`; 0, 3010, 1641 e 1638 = sucesso) |
  | `fs.set_case_sensitive {path}` | diretório dentro da raiz (exceto a própria raiz, `bin/` e `state/`) ou dentro de um parked dir ([15](15-linux-parity-case-sensitivity.md) §3) |
- **Protocolo** ([ADR-0022](adr/0022-elevated-helper-protocol.md), `forge_core::helper`): a allowlist é o enum fechado `HelperOp` (`{"name": "nrpt.add", "params": {...}}`, campos desconhecidos recusados); `HelperOp::validate` roda **no daemon** (recusa antes de qualquer UAC) e **de novo no helper**, que lê TLDs e parked dirs do `etc/forgebox.toml` da própria raiz (nunca do pedido) e só aceita `root` igual à pasta onde está. Os sufixos de uso local protegem contra um `forgebox.toml` adulterado por processo do usuário (`tld = ".com"` não vira regra NRPT). Pedido `HelperRequest {v, request_id, root, op}` e resposta `HelperResponse {v, request_id, ok, code, message, details}` em JSON (`v = 1`). Como stdin/stdout não atravessam o `runas`, o daemon cria antes `state/locks/helper-<request_id>.request.json` e `.response.json` com DACL protegida (usuário + `BUILTIN\Administrators`, para elevação com outra conta), o helper lê o primeiro e escreve no segundo (só abre, nunca cria; recusa reparse point e hard link), e o daemon apaga os dois. UAC recusado → `needs_elevation`.
- **Execução** ([ADR-0024](adr/0024-helper-execution.md), crate `forge-helper`, `forge_core::platform::PrivilegedOps`): sem shell — NRPT por WMI `PS_DnsClientNrptRule` (`root/Microsoft/Windows/DNS`), Defender por WMI `MSFT_MpPreference`, firewall por COM `INetFwPolicy2`, `LocalMachine\Root` por CryptoAPI, `hosts` por `ReplaceFileW` (fora do bloco nada muda, byte a byte, em qualquer codificação), flush por `DnsFlushResolverCache`. Regras NRPT do Forgebox têm `Comment = "Forgebox"`; com regras de política de grupo o helper grava só a regra local e responde `effective: false` (o daemon cai para o `hosts`). Os caminhos são validados de novo depois de resolver junctions; pedido, resposta, `helper.log`, certificados e instalador são abertos sem seguir links (recusa reparse point e hard link); `helper.log` é aberto antes de tudo (sem log, sem ação); sem `--dry-run` o helper exige token elevado. `--dry-run` valida, lê o estado atual e devolve o plano sem mudar nada nem exigir elevação. Códigos de saída: 0 ok, 1 recusado/falhou (resposta escrita), 2 uso incorreto (nada escrito), 3 resposta não escrita.
- **`forgebox-ctrl.exe`** é um auxiliar interno do daemon, **sem** elevação e sem rede: só se anexa ao console de um serviço do próprio usuário e gera `CTRL_BREAK` para o grupo dele (`forgebox-ctrl break <pid>`; [ADR-0016](adr/0016-ctrl-break-helper-process.md)).
- O helper é invocado por processo (ShellExecute `runas`, um UAC por operação ou por lote agrupado no onboarding). Modo opcional "serviço helper" (como HerdHelper) para quem não quer UAC: serviço Windows ouvindo em named pipe com ACL restrita ao SID do usuário instalador, mesma allowlist. Padrão: **sem serviço**.
- Tudo que o helper faz vai para `logs/helper.log` e para o histórico de eventos.

## 2. Cadeia de confiança dos downloads
1. Registry oficial assinado (minisign) e verificado antes de qualquer manifesto ser lido (`forge-registry`, [04](04-tool-manifest-spec.md) §5); sha256 de cada manifesto conferido contra o índice assinado; cache reverificado a cada leitura. Registries de terceiros exibem aviso ("terceiro", e "não verificado" quando sem assinatura, só com `trusted_unsigned`) e não podem declarar `post_install.run` (flag `official` por registry). Uma assinatura errada nunca é aceita, nem em `trusted_unsigned`. A chave de dev (`registry/keys/dev.pub`) só vale para desenvolvimento; a chave de produção é embutida na release (§7).
2. Cada download: SHA-256 obrigatório (do upstream ou curado no registry pelo bot, com PR revisado). Sem hash → bloqueado por padrão.
3. Assinaturas upstream verificadas quando existem (PGP nginx/Node com keyrings embutidos e fingerprints fixados por teste; Authenticode sem checagem de revogação, para funcionar offline). Ver [ADR-0017](adr/0017-verify-extract-stack.md).
4. Staging em `tmp/stage` e extração com proteção contra zip-slip: o arquivo inteiro é recusado se alguma entrada tiver `..`, caminho absoluto, letra de drive, `:` (ADS), caractere proibido no Windows, nome reservado (`CON`, `NUL.txt`, `COM1`...), componente terminado em `.`/espaço, link simbólico ou físico, reparse point, device/fifo ou dados criptografados; limites de bytes escritos e de número de entradas contra bombas de descompressão. A extração nunca usa `extract()`/`unpack()` das bibliotecas.
5. `post_install` só executa binários dentro da própria versão instalada.

## 3. CA local
Detalhes e medições em [ADR-0023](adr/0023-local-ca-name-constraints-trust.md).
- Chave privada da CA em `certs/ca/forgebox-ca.key` com ACL só do usuário (DACL protegida com um único ACE, aplicada ao arquivo ainda vazio, antes do segredo); nunca sai da máquina; nunca é lida pela GUI nem devolvida pela API (só o daemon a carrega, em memória do ator `tls`). Chaves dos sites (`certs/sites/<host>.key`) recebem a mesma ACL.
- Instalação em `CurrentUser\Root` (não afeta outros usuários) por padrão. `LocalMachine` só via helper e opt-in explícito. Modo portátil nunca toca no store.
- Name constraints **críticas** no certificado da CA (`permittedSubtrees`: `dNSName` de cada TLD configurado **de uso local** — `.test`, `.localhost`; um `.dev` no `forgebox.toml` fica de fora, o mesmo filtro do helper —, `localhost`, `iPAddress` `127.0.0.0/8` e `::1/128`) e `pathLenConstraint = 0`, para que, mesmo vazando a chave, ela não possa assinar `google.com` nem uma CA intermediária. Verificado por teste no webpki (rustls), no CryptoAPI do Windows (o que Schannel/`curl.exe`/Chrome/Edge usam) e no OpenSSL (`permitted subtree violation`). Isso é algo que mkcert e Herd **não** fazem.
- Rotação em 1 clique, com a nova CA confiada antes de qualquer arquivo mudar; thumbprints SHA-1 de toda raiz adicionada ficam em `certs/ca/trust.json` para remoção limpa (rotação e desinstalação).
- CRL vazia assinada pela CA servida só em `127.0.0.1` (porta da faixa `tls-crl`, padrão 47480), só para a checagem de revogação do Schannel passar; não expõe nada além de um documento público.

## 4. API local (IPC)
- Named pipe `\\.\pipe\forgebox` (raiz padrão; outras raízes usam `\\.\pipe\forgebox-<hash da raiz>`) com descritor `O:<SID>D:P(A;;GA;;;<SID>)`: dono e único acesso = usuário atual, `PIPE_REJECT_REMOTE_CLIENTS` e primeira instância exclusiva (`FILE_FLAG_FIRST_PIPE_INSTANCE`: um segundo daemon ou um posseiro não compartilha o nome). O cliente confere que o pipe pertence ao usuário atual antes de enviar qualquer coisa (`forge_core::platform::windows::pipe_security`).
- Toda conexão começa com `system.hello {token, client, api_version}`; o token (256 bits, `state/ipc.token`) é criado pelo daemon: arquivo vazio → DACL protegida só do usuário (`FileAcl`) → segredo escrito → rename. No pipe é defesa em profundidade; token errado fecha a conexão. Conexão local = papel `admin`. Detalhes em [ADR-0015](adr/0015-ipc-protocol.md).
- WebSocket (`127.0.0.1:<porta>`) desligado por padrão; quando ligado exige o mesmo token em header. Sem CORS para origens não-locais; a GUI Tauri usa o pipe.
- `fs.*` da API só alcança `etc/`, `logs/` e `www/` (validação de sintaxe + prefixo canônico; links que escapam são recusados com `forbidden`).
- MCP server (`forge mcp serve`) via stdio apenas; ferramentas destrutivas (`remove`, `drop`) exigem `confirm: true` explícito no payload.

## 5. Serviços expostos
- Todos os serviços escutam em `127.0.0.1` por padrão (Laragon/XAMPP frequentemente usam `0.0.0.0`). Expor na LAN é ação explícita por serviço/site, com regra de firewall nomeada e aviso. O próprio daemon escuta em `127.0.0.1` para o resolvedor DNS (`:53`) e para a CRL da CA local (`:47480`, só `GET` de um caminho).
- MySQL/MariaDB: `root` sem senha **só em 127.0.0.1** (prática de dev), mas o Forgebox gera um usuário `forgebox` com senha aleatória para uso das próprias ferramentas e oferece "definir senha de root" no onboarding.
- PostgreSQL: `trust` apenas para `127.0.0.1/32`.
- Mailpit/MinIO/Meilisearch: credenciais padrão geradas aleatoriamente e exibidas/copiáveis (não `minioadmin/minioadmin`).

## 6. Segredos na UI
- `.env` viewer mascara chaves que casem `(KEY|SECRET|PASSWORD|TOKEN)`; "revelar" é por item e temporário.
- Logs de processes passam por redator básico para `Authorization:`/`password=`.

## 7. Supply chain do próprio Forgebox
- A chave de produção do registry oficial é embutida na release no lugar de `registry/keys/dev.pub`; o segredo fica fora do repositório (CI de release). `cargo xtask registry-sign` assina o `index.yaml` com uma assinatura minisign destacada (`index.yaml.minisig`, [ADR-0018](adr/0018-detached-index-signature.md)) (a chave de dev secreta fica em `Forgebox-wt/cache/keys/dev.key`, fora do git).
- `cargo-deny` (licenças, advisories), `cargo-audit` em CI, lockfile commitado, builds reprodutíveis em GitHub Actions com SLSA provenance, binários assinados com certificado Authenticode do projeto (quando disponível) + minisign sempre.
- Telemetria: **desligada por padrão**; opt-in; só contadores agregados (versão, SO, features usadas); nunca hosts, caminhos ou IPs. Documentado na UI com o payload exato.

## 8. Ameaças consideradas e mitigação
| Ameaça | Mitigação |
|---|---|
| Registry comprometido injeta URL maliciosa | Assinatura minisign do index; hash por manifesto; PR review + bot |
| Zip malicioso com path traversal | Extrator rejeita; staging isolado |
| Processo local de outro usuário fala com o daemon | DACL do pipe |
| Site local malicioso usa a CA para MITM | Name constraints críticas na CA (só TLDs de uso local, `localhost`, loopback) e `pathLen 0` |
| Chave da CA lida por outro usuário da máquina | DACL só do usuário em `forgebox-ca.key`, aplicada antes de o segredo ser escrito |
| Pool PHP acessível por outras máquinas | Bind 127.0.0.1 em tudo; firewall só explícito |
| Helper elevado usado como escada de privilégio | Allowlist fechada, validação estrita, sem shell, log |
| `forgebox.toml` adulterado (`tld = ".com"`) para desviar o DNS da máquina via NRPT/`hosts` | Helper só roteia/mapeia nomes sob sufixos de uso local, só para loopback ([ADR-0022](adr/0022-elevated-helper-protocol.md)) |
| Link plantado em `state/locks/` para o helper elevado escrever fora | Arquivos de troca criados pelo daemon antes do lançamento; helper só abre o existente, recusa reparse point e hard link |
| Junction em `bin/`, `tmp/downloads/` ou `logs/` para o helper agir fora da raiz | Caminhos revalidados depois de `canonicalize`; `helper.log` conferido pelo caminho final do handle ([ADR-0024](adr/0024-helper-execution.md)) |
| Instalador trocado entre a verificação e a execução | Cópia para pasta só de `SYSTEM`/`Administrators`, sha256 e Authenticode da cópia, execução da cópia |
| CA arbitrária colocada em `certs/ca/` para virar raiz de todas as contas | `trust.add_machine_root` só aceita CA Forgebox autoassinada com Name Constraints de uso local |
| Update do app substituído | Updater exige assinatura minisign; canal HTTPS |

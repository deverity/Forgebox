# ADR-0022 — Protocolo do `forgebox-helper`: allowlist tipada, JSON versionado, troca por arquivos em `state/locks/`

**Status:** aceito · **Data:** 2026-10-27 · Execução das operações no helper: [ADR-0023](0023-helper-execution.md) (que substitui a obrigação "gravar a regra NRPT na tabela efetiva" abaixo: com regras de GPO o helper grava só a regra local e informa `effective: false`; o daemon usa o `hosts`).

## Contexto
O `forgebox-helper.exe` é o único binário elevado ([11 §1](../11-security.md)). A primeira operação que precisa dele é a regra NRPT de `.test` ([ADR-0004](0004-dns-nrpt-resolver.md)); depois vêm `hosts`, CA em `LocalMachine`, firewall, Defender, VC++ e a flag case-sensitive. Três perguntas precisavam de resposta antes do binário existir (M1-E5-T4):

1. **O que** o helper aceita e como validar, de modo que o daemon recuse antes de mostrar um UAC e o helper recuse de novo mesmo se chamado por outro processo.
2. **Como** o pedido e a resposta atravessam a fronteira de elevação. `ShellExecuteExW` com verbo `runas` cria o processo pelo serviço AppInfo: stdin/stdout/stderr **não** são herdados, e o elevado pode até rodar sob outra conta (elevação "over-the-shoulder": usuário padrão digita a senha de um administrador).
3. **Quem** define a política (TLDs, raiz): `etc/forgebox.toml` é gravável pelo usuário, a política de DNS da máquina não é.

## Decisão
**Allowlist tipada em `forge_core::helper`** (compartilhada por daemon e helper):
- `HelperOp` é um enum fechado com uma variante por operação de [11 §1](../11-security.md) (`nrpt.add`, `nrpt.remove`, `hosts.set_block`, `trust.add_machine_root`, `trust.remove_machine_root`, `firewall.add`, `firewall.remove`, `defender.exclude`, `defender.remove`, `vcredist.install`, `fs.set_case_sensitive`). No fio: `{"name": "nrpt.add", "params": {...}}`, `deny_unknown_fields` em tudo: nome desconhecido ou campo a mais não desserializa (`forbidden` / `invalid_input`).
- `HelperOp::validate(&HelperPolicy)` é puro e léxico: namespaces e hosts só sob TLDs configurados **e** sob sufixos reservados a uso local (`dns_name::LOCAL_USE_SUFFIXES`: `test`, `localhost`, `invalid`, `example`, `local`, `internal`, `home.arpa`, `lan`, `home`, `corp`); NRPT só para `127.0.0.1`/`127.0.0.53`; `hosts` só `127.0.0.1`/`::1`, no máximo 4096 linhas; executáveis só dentro de `<root>/bin`; regras de firewall só `Forgebox-[A-Za-z0-9._-]{1,64}`; Defender só a própria raiz; instalador do VC++ só em `<root>/tmp/downloads`; flag case-sensitive só dentro da raiz (fora de `bin/` e `state/`) ou de um parked dir. Caminhos são comparados por componentes, sem caixa no Windows, recusando `..`, relativos, ADS (`:`) e componentes terminados em `.`/espaço.
- `HelperClient` (daemon) valida, monta o `HelperRequest` com o `request_id` da chamada da API, chama a `platform::Elevation` e confere a resposta (mesmo `request_id`, `v`); recusa vira o `ForgeError` do `code` devolvido. `FakeElevation` substitui o UAC nos testes.

**Documentos** (`HelperRequest {v, request_id, root, op}` / `HelperResponse {v, request_id, ok, code?, message, details}`), JSON UTF-8, `v = 1`, no máximo 1 MiB. Daemon e helper são da mesma release; versão diferente é instalação quebrada e é recusada.

**Transporte (Windows, `platform::windows::elevation`):** um processo por pedido.
1. O daemon cria `<root>/state/locks/helper-<request_id>.request.json` e `.response.json` com `CREATE_NEW`, aplica DACL protegida `D:P(A;;FA;;;<SID do usuário>)(A;;FA;;;BA)` e só então escreve o pedido.
2. `ShellExecuteExW("runas", forgebox-helper.exe, --request <arq> --response <arq>)` com `SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI`, `SW_HIDE`, COM inicializado (STA). UAC recusado = `ERROR_CANCELLED` → `needs_elevation`.
3. Espera o processo (padrão 10 min após o consentimento; a espera do próprio prompt não tem limite, o job do daemon tem), lê o código de saída e a resposta; resposta vazia ou inválida → `internal` com o código de saída. Os dois arquivos são apagados sempre.

**Obrigações do helper (M1-E5-T4):** aceitar só `request.root` igual à própria raiz (pasta do `forgebox-helper.exe`); ler TLDs e parked dirs do `etc/forgebox.toml` **dessa** raiz, nunca do pedido; exigir que o pedido esteja em `<root>/state/locks/helper-<request_id>.request.json`; abrir a resposta com `OPEN_EXISTING`, recusando reparse point ou mais de um hard link; canonicalizar caminhos de novo antes de agir; registrar tudo em `logs/helper.log`. Para NRPT, quando existirem regras de política de grupo (que fazem o Windows ignorar as locais), gravar a regra na tabela efetiva.

## Alternativas rejeitadas
- **Pedido na linha de comando:** limite de tamanho (um `hosts.set_block` com centenas de sites passa de 32 KiB) e a linha de comando de um processo elevado é visível a todos os usuários.
- **Named pipe criado pelo daemon:** evita arquivos, mas o processo elevado vira cliente de um pipe cujo servidor é o usuário comum (risco de impersonação se o helper esquecer `SECURITY_IDENTIFICATION`) e o nome pode ser ocupado antes por outro processo do usuário. Arquivos com DACL criados antes do lançamento são mais simples de auditar e de testar.
- **Arquivos só com a ACL do usuário:** quebram a elevação over-the-shoulder (o administrador que roda o helper não lê o pedido). Por isso `BUILTIN\Administrators` também recebe acesso.
- **Validar só no helper:** cada recusa custaria um prompt UAC inútil. Validar só no daemon: o helper pode ser chamado por qualquer processo do usuário.
- **Confiar nos TLDs do `forgebox.toml` sem filtro:** um processo do usuário poderia escrever `tld = ".com"` e pedir `nrpt.add`, desviando o DNS da máquina inteira para um servidor que ele mesmo sobe em `127.0.0.53:53`. Daí a lista fixa de sufixos de uso local.
- **Lote de operações por pedido (vários `op` num UAC):** útil no onboarding, mas não necessário para o DNS (um TLD padrão). Fica para uma `v = 2`, sem quebrar a `v = 1`.

## Consequências
- Um UAC por operação; `forge dns enable` com só `.test` mostra um prompt (nenhum se a regra já existe).
- `etc/forgebox.toml` com TLD público (`.dev`) continua válido para o resolver, mas o helper recusa NRPT/`hosts` para ele (`forbidden`).
- O transporte é testado de ponta a ponta sem UAC: `WindowsElevation::without_elevation()` usa o mesmo caminho com verbo `open` contra um helper falso (`crates/forge-core/tests/fake_helper`); só o prompt real fica para o smoke manual.
- Remoto ([13](../13-remote-mode.md)): o prompt aparece na máquina do daemon; métodos que elevam continuam exigindo alguém diante dela.

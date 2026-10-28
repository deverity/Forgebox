# 16 — Smoke do gate M1 (`scripts/smoke.ps1`)

O gate M1 ([10](10-roadmap-and-agent-tasks.md)) é um script que, numa raiz nova, faz tudo pela CLI: instala `php@8.4` + `nginx` + `mysql@8.4` + `mailpit` (+ `composer`), cria o site Laravel `loja`, abre `https://loja.test`, roda `php artisan migrate` e confere no Mailpit um e-mail enviado pelo Laravel. Decisões e correções que ele motivou: [ADR-0032](adr/0032-m1-gate-integration.md).

## Uso
```powershell
# PowerShell 7, na raiz do repositório. Compila os binários (cargo build) e roda.
pwsh -NoProfile -File scripts/smoke.ps1                    # interativo (UAC + diálogo da CA)
pwsh -NoProfile -File scripts/smoke.ps1 -NonInteractive    # sem elevação nem diálogos (CI, máquina sem usuário)
pwsh -NoProfile -File scripts/smoke.ps1 -NonInteractive -Keep -Root D:\tmp\fb-smoke
```

| Parâmetro | Padrão | O que faz |
|---|---|---|
| `-Root` | `%TEMP%\fb-smoke-<data-hora>` | Raiz Forgebox nova (precisa não existir ou estar vazia). |
| `-Keep` | — | Mantém a raiz no fim (senão é apagada e o log é copiado para `<raiz>.smoke.log` ao lado). |
| `-NonInteractive` | — | Nada eleva nem abre diálogo (ver abaixo). |
| `-HttpPort` / `-HttpsPort` | 80 / 443 | Portas do nginx. |
| `-MysqlPort` | 3306 | Porta do MySQL. |
| `-MailpitSmtpPort` / `-MailpitHttpPort` | 1025 / 8025 | SMTP e UI/API do Mailpit. |
| `-LogPath` | `<raiz>\logs\smoke.log` | Log completo (toda saída dos comandos). |
| `-NoBuild` | — | Usa o que já está em `target\debug` (sem `cargo build`). |
| `-LaravelPackage` | `laravel/laravel` | Esqueleto do `composer create-project`. |
| `-BreakForgeBeforeTeardown` | — | Autoteste do teardown: troca o `forge.exe` da raiz por um arquivo não executável logo antes do passo 7, que precisa sair `FAIL` mas matar mesmo assim todo processo da raiz (`leftovers killed: N`, nada vivo depois). |

Se uma porta pedida está ocupada (nesta máquina o Laragon usa 80/443/3306), o script escolhe a primeira livre entre `porta + 10000·k` e avisa. As portas vão para o Forgebox **pela CLI** (`forge config set ports.ranges.web "[http, https]"`, `ports.ranges.databases`, `ports.ranges.mail`), que aplica na hora ([03 §4](03-engine.md)). As portas fixas do próprio daemon (responder de CRL 47480, endpoint interno 47481) só mudam se outro processo as ocupa: `forge config set ports.ranges.tls-crl|internal` + `forge daemon restart` (são abertas no boot).

## O que ele faz
1. **Raiz e binários:** cria a raiz no formato da release (`forge.exe`, `forgeboxd.exe`, `forgebox-ctrl.exe`, `forgebox-helper.exe` do workspace copiados para ela, [08 §1](08-release-layout.md)); o registry oficial é a pasta `registry/` do repositório (assinada com a chave de dev, `FORGEBOX_OFFICIAL_REGISTRY`). `FORGE_LANG=en`, `NO_COLOR=1`.
2. `forge daemon start` contra a raiz.
3. Portas (acima).
4. `forge tool install php@8.4 nginx mysql@8.4 mailpit composer` (sha256 obrigatório, como sempre).
5. `forge start all` e conferência de estados e portas em `forge status --json`.
6. `forge exec -- composer create-project laravel/laravel <raiz>\projects\loja` (Composer e PHP do Forgebox; TLS pelo bundle de CAs, [06 §6.5](06-sites-vhosts-multi-php.md)).
7. `forge site add <projeto> --host loja.test --php 8.4 --server nginx --driver laravel --secure`.
8. Banco `loja` criado com o cliente `mysql` do Forgebox (`forge exec -- mysql ... -e "CREATE DATABASE ..."`; `forge db create` é M2-E3) e `.env` do projeto apontado para o MySQL e o SMTP do Mailpit nas portas reais (o `.env` é do app; nenhuma configuração do Forgebox é escrita à mão).
9. `forge exec --php 8.4 -- php artisan migrate --force` e conferência da tabela `migrations`.
10. HTTPS (no interativo, antes: o resolvedor do SO — `Dns.GetHostAddresses`, o `getaddrinfo` do `curl` — precisa responder `loja.test` só com loopback em até 15 s, com `Clear-DnsClientCache` entre tentativas; se não, o passo `5a` falha mostrando `forge dns status`): `curl.exe` (Schannel, revogação checada pela CRL do Forgebox) em `https://loja.test[:porta]/` → 200 com a página do Laravel; e .NET confere que o certificado servido encadeia na CA do Forgebox (`certs\ca\forgebox-ca.crt`) como única âncora e que o emissor é ela.
11. E-mail: um comando artisan (`smoke:mail`, acrescentado em `routes/console.php` do projeto) envia um e-mail com assunto único por execução; a API do Mailpit (`/api/v1/messages`) precisa mostrá-lo.
12. DNS: `forge dns status` (resolvedor rodando) e, no modo não interativo, `Resolve-DnsName loja.test -Server 127.0.0.1` → `127.0.0.1`; no interativo, `forge dns test loja.test`.
13. **Teardown** (sempre, em `finally`): `forge dns disable` (só no interativo), `forge stop all` e `forge daemon stop`, cada um no seu `try/catch` (um `forge.exe` que nem abre — antivírus, arquivo apagado — não pula os seguintes); depois, **sempre**, mata os processos cujo executável está dentro da raiz (e só eles; nada do Laragon nem de outra raiz), confere que nenhum sobrou e só então apaga a raiz (salvo `-Keep`). Qualquer ação que falhou deixa o passo `FAIL`, mas a limpeza acontece do mesmo jeito.

Cada passo sai com tempo e `PASS`/`FAIL`/`SKIP` (um passo que falha faz os dependentes virarem `SKIP`); no fim, um resumo e o código de saída (≠ 0 se algo falhou).

## Modos
- **Interativo (padrão, máquina limpa com uma pessoa na frente):** raiz **instalada** (sem `portable.flag`: o modo portátil nunca toca o store de confiança, [08 §2.2](08-release-layout.md)); `forge dns enable` (UAC para a regra NRPT de `.test`; outro UAC para o bloco do `hosts` se o Windows ignorar regras locais — chave de política de grupo presente, mesmo vazia — ou não seguir a regra, [ADR-0033](adr/0033-nrpt-group-policy-table-and-resolver-probe.md)) e `forge tls ca install` (diálogo do Windows para `CurrentUser\Root`); o log registra o modo efetivo do DNS (`nrpt` ou `hosts`, com motivo e sonda) e o passo `5a` espera o resolvedor do SO; o `curl` usa o resolvedor e o store do sistema, sem `--resolve` nem `--cacert`. No teardown, `forge dns disable` (remove a regra local e o bloco do `hosts`). O script nunca roda `forge path add`: o `PATH` do usuário não muda. A CA instalada fica no `CurrentUser\Root` (remova pelo `certmgr.msc` se quiser).
- **`-NonInteractive`:** raiz portátil (`portable.flag`: sem PATH, sem registro, sem store); nada eleva nem abre diálogo. `loja.test` é resolvido por `curl --resolve loja.test:<porta>:127.0.0.1` e a cadeia validada com `--cacert <raiz>\certs\ca\forgebox-ca.crt` — **nunca `-k`**. O servidor DNS do Forgebox é consultado diretamente em `127.0.0.1:53`.

## Problemas conhecidos
- **Falso positivo do antivírus no build de debug.** O heurístico de ML do Windows Defender já classificou o `forge.exe` de debug (sem assinatura) copiado para `%TEMP%` como `Trojan:Win32/Bearfoos.A!ml` (1 em 4 execuções numa máquina) e o pôs em quarentena — às vezes também o `target\debug\forge.exe`. Sintoma: um passo falha com "o arquivo contém um vírus" / "Operation did not complete successfully because the file contains a virus". Confira com `Get-MpThreatDetection | Sort-Object InitialDetectionTime | Select-Object -Last 3`; recompile (`cargo build -p forge-cli`) e rode de novo, de preferência com `-Root` fora de `%TEMP%` (ex.: `-Root C:\fb-smoke`). Não é bug do produto: os binários de release serão assinados (M4). O teardown continua limpando a raiz nesse caso (item 13).
- **Raiz em `C:\` numa máquina gerenciada.** Onde o usuário cria pastas mas não arquivos em `C:\` (a ACL herdada dá só *Modify*, sem `FILE_DELETE_CHILD`), uma raiz como `C:\fb-smoke-ni` não recebe a flag case-sensitive ([15 §5](15-linux-parity-case-sensitivity.md)). O daemon lê a flag real do data dir: o MySQL do smoke sobe com `lower_case_table_names=2` e é publicado `fs.case_sensitivity_unavailable` (antes da correção o passo 2 falhava no `mysqld --initialize`). O log final, que iria para `C:\fb-smoke-ni.smoke.log`, vai para `%LOCALAPPDATA%\Forgebox\smoke\` (tabela de parâmetros). Para testar a paridade case-sensitive, use uma raiz sob `%LOCALAPPDATA%`.

## Resultado nesta máquina (2026-10-27)
Windows 10 Enterprise com Laragon rodando (Apache 80/443, MySQL 3306) e inspeção TLS corporativa: `-NonInteractive` passa de ponta a ponta em ~2 min (nginx 10080/10443, MySQL 13306, Mailpit 1025/8025; PHP 8.4.27, nginx 1.31.8, MySQL 8.4.13, Mailpit 1.32.0, Composer 2.10.3, Laravel 13). Modo interativo, primeira execução (máquina em domínio corporativo, com VPN): tudo passou menos a resolução de `loja.test` — a GPO deixou a chave de política NRPT vazia, o Windows ignorou a regra local e o Forgebox respondia "aplicada". Corrigido pela [ADR-0033](adr/0033-nrpt-group-policy-table-and-resolver-probe.md): nesta máquina `forge dns status` passa a mostrar `Modo efetivo .test: nenhum`, motivo `group_policy_table_present`, e `forge dns enable` vai para o `hosts`. Nova execução interativa pendente.

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

Se uma porta pedida está ocupada (nesta máquina o Laragon usa 80/443/3306), o script escolhe a primeira livre entre `porta + 10000·k` e avisa. As portas vão para o Forgebox **pela CLI** (`forge config set ports.ranges.web "[http, https]"`, `ports.ranges.databases`, `ports.ranges.mail`), que aplica na hora ([03 §4](03-engine.md)).

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
10. HTTPS: `curl.exe` (Schannel, revogação checada pela CRL do Forgebox) em `https://loja.test[:porta]/` → 200 com a página do Laravel; e .NET confere que o certificado servido encadeia na CA do Forgebox (`certs\ca\forgebox-ca.crt`) como única âncora e que o emissor é ela.
11. E-mail: um comando artisan (`smoke:mail`, acrescentado em `routes/console.php` do projeto) envia um e-mail com assunto único por execução; a API do Mailpit (`/api/v1/messages`) precisa mostrá-lo.
12. DNS: `forge dns status` (resolvedor rodando) e, no modo não interativo, `Resolve-DnsName loja.test -Server 127.0.0.1` → `127.0.0.1`; no interativo, `forge dns test loja.test`.
13. **Teardown** (sempre, em `finally`): `forge stop all`, `forge daemon stop`, mata só processos cujo executável está dentro da raiz, apaga a raiz (salvo `-Keep`).

Cada passo sai com tempo e `PASS`/`FAIL`/`SKIP` (um passo que falha faz os dependentes virarem `SKIP`); no fim, um resumo e o código de saída (≠ 0 se algo falhou).

## Modos
- **Interativo (padrão, máquina limpa com uma pessoa na frente):** raiz **instalada** (sem `portable.flag`: o modo portátil nunca toca o store de confiança, [08 §2.2](08-release-layout.md)); `forge dns enable` (UAC para a regra NRPT de `.test`) e `forge tls ca install` (diálogo do Windows para `CurrentUser\Root`); o `curl` usa o resolvedor e o store do sistema, sem `--resolve` nem `--cacert`. No teardown, `forge dns disable`. O script nunca roda `forge path add`: o `PATH` do usuário não muda. A CA instalada fica no `CurrentUser\Root` (remova pelo `certmgr.msc` se quiser).
- **`-NonInteractive`:** raiz portátil (`portable.flag`: sem PATH, sem registro, sem store); nada eleva nem abre diálogo. `loja.test` é resolvido por `curl --resolve loja.test:<porta>:127.0.0.1` e a cadeia validada com `--cacert <raiz>\certs\ca\forgebox-ca.crt` — **nunca `-k`**. O servidor DNS do Forgebox é consultado diretamente em `127.0.0.1:53`.

## Resultado nesta máquina (2026-10-27)
Windows 10 Enterprise com Laragon rodando (Apache 80/443, MySQL 3306) e inspeção TLS corporativa: `-NonInteractive` passa de ponta a ponta em ~2 min (nginx 10080/10443, MySQL 13306, Mailpit 1025/8025; PHP 8.4.27, nginx 1.31.8, MySQL 8.4.13, Mailpit 1.32.0, Composer 2.10.3, Laravel 13). O modo interativo depende de alguém aceitar o UAC e o diálogo da CA e não foi executado aqui.

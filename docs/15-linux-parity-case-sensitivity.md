# 15 — Paridade com Linux: case-sensitive por padrão

## 1. Problema
Windows é case-insensitive no sistema de arquivos. Um projeto funciona no Laragon com `require 'Views/header.php'` apontando para `views/Header.php`, uma classe `App\Models\User` em `app/models/user.php`, uma imagem `Logo.PNG` referenciada como `logo.png`, e **quebra ao publicar em Linux**. O desenvolvedor só descobre em produção. O mesmo vale para nomes de tabelas MySQL, rotas, assets e `require` do Node.

## 2. Decisão
**Forgebox é case-sensitive por padrão em tudo que controla**, e o usuário pode desativar globalmente ou por site em Configurações → Sistema de arquivos → "Modo case-sensitive (paridade Linux)". Ver [ADR-0010](adr/0010-case-sensitive-by-default.md).

## 3. Mecanismo: case sensitivity por diretório do NTFS
Desde o Windows 10 1803 o NTFS suporta a flag **por diretório** `FILE_CS_FLAG_CASE_SENSITIVE_DIR`, a mesma usada pelo WSL. Dentro de um diretório marcado, `Header.php` e `header.php` são arquivos distintos e `open("header.php")` falha se só existir `Header.php`, exatamente como no Linux. Novos subdiretórios **herdam** a flag.

- API: `NtSetInformationFile(handle, FileCaseSensitiveInformation { Flags: FILE_CS_FLAG_CASE_SENSITIVE_DIR })` (via `windows` crate / `ntdll`). Equivalente de linha de comando: `fsutil file setCaseSensitiveInfo <dir> enable`.
- Pré-condições: NTFS (não FAT/exFAT/ReFS), Windows 10 1803+, permissão de escrita de atributos no diretório. Se o sistema exigir elevação em alguma edição/política, a operação é roteada pelo `forgebox-helper` (op `fs.set_case_sensitive {path}` validada para estar dentro de parked dirs/raiz do Forgebox, nunca a própria raiz, `bin/` ou `state/`; `forge_core::helper::HelperOp`, [ADR-0021](adr/0021-elevated-helper-protocol.md)).
- Comportamento medido no Windows 10 22H2 (19045), NTFS, sem elevação (M0-E1-T4): a flag **pode** ser ligada em diretório não vazio, mas vale **só para o próprio diretório** (não é recursiva): subdiretórios já existentes mantêm a flag que tinham e só os criados depois herdam. Desligar falha com `STATUS_CASE_DIFFERING_NAMES_IN_DIR` enquanto houver nomes que diferem só por caixa. **Permissão exigida (medido em M1-E1-T2):** `NtSetInformationFile` com `FileCaseSensitiveInformation` falha com `STATUS_ACCESS_DENIED` (0xC0000022) se o usuário não tiver `FILE_DELETE_CHILD` ("Excluir subpastas e arquivos") no diretório, mesmo tendo `FILE_WRITE_ATTRIBUTES`. Diretórios sob `%TEMP%` (dono com Controle Total) funcionam; diretórios criados sob uma raiz cuja ACL herdada dá só *Modify* ao usuário (ex. `C:\apps_src\...` neste ambiente) falham. `fsutil file setCaseSensitiveInfo` exige elevação. Consequência: `ForgeRoot::init()` em uma raiz assim deve conceder `(DC)` ao usuário no diretório recém-criado (`icacls <dir> /grant <user>:(OI)(CI)(DC)`) antes de marcar, ou rotear por `fs.set_case_sensitive` do helper (`needs_elevation`). Por isso o Forgebox marca diretórios **vazios, antes de criar conteúdo** (`ForgeRoot::init()`), e a conversão de diretórios existentes (§3.1) é "mover para um diretório novo já marcado", o que torna toda a subárvore case-sensitive.
- Implementação em `forge-core::platform::windows::WindowsCaseSensitiveFs` (`NtQueryInformationFile`/`NtSetInformationFile`, `FileCaseSensitiveInformation` = classe 71) atrás do trait `platform::CaseSensitiveFs { support(dir) / is_supported(dir), get(dir), set(dir, bool) }`. Erros tipados: `unsupported_platform` (volume/SO sem suporte, nunca pânico), `needs_elevation` (acesso negado → helper), `conflict` (nomes que diferem só por caixa). Em Linux/macOS (port futuro): `get` devolve o estado real do FS; `set` é no-op; até lá o stub responde `unsupported_platform`.

### 3.1 Onde aplicamos
| Diretório | Quando | Como |
|---|---|---|
| `www/` (parked padrão) e qualquer parked dir criado pelo Forgebox | Onboarding (criado vazio → flag antes de qualquer arquivo) | direto |
| Parked dir **existente** com conteúdo (ex. `C:\laragon\www` importado) | `forge park` / import | Oferece **Converter**: cria `<dir>.cs-tmp` vazio com a flag, move cada entrada (rename no mesmo volume, instantâneo), troca nomes, verifica. Reversível. Avisa sobre arquivos abertos |
| Projeto linkado fora de parked dir (`forge site add D:\x\proj`) | Ao criar o site | Diagnóstico mostra "diretório case-insensitive"; botão **Converter** igual acima; pode recusar |
| `www/<novo projeto>` via `forge new` | Herdado automaticamente | — |
| `data/` dos bancos | Na criação da instância (vazio) | direto (ver §5) |
| `etc/`, `logs/`, `tmp/`, `backups/` | Onboarding (`ForgeRoot::init()`, que também marca as subpastas que cria: `etc/templates`, `etc/sites`, `tmp/downloads`, `tmp/stage`) | direto, por consistência |
| `bin/` | **Não** | Tools Windows esperam FS normal; sem benefício. A extração (`forge_provider::extract`) dá ao staging (`tmp/stage/<id>`, que herdaria a flag de `tmp/`) a sensibilidade do pai do destino antes de escrever, porque a flag viaja com o diretório no rename final |

Em cada site a UI mostra badge **`Aa` case-sensitive** ou **`aA` insensitive** com tooltip explicando. O estado é re-verificado no boot (`fs.case_sensitivity_applied|unavailable`).

### 3.2 Quando não é possível
Volume não-NTFS, Windows < 1803, política de grupo bloqueando, diretório não vazio e usuário recusou converter: o site fica em modo insensitive, com badge e um aviso **não intrusivo** no Diagnóstico. O **Case Mismatch Finder** (§6) continua disponível como rede de segurança.

## 4. Paridade no servidor web e no PHP
- **Templates sem `~*`**: locations/rewrites gerados usam regex case-sensitive (`~`), como se escreve para Linux; `.htaccess` do usuário é respeitado como está.
- `mod_speling`/`CheckSpelling` nunca habilitado. Em Nginx não existe equivalente.
- Apache no Windows compara `<Directory>`/`<Files>` sem diferenciar caixa (comportamento do core); o acesso real ao arquivo segue o FS, então em diretório CS um `GET /Index.php` devolve 404 como no Linux. Documentado como limitação do Apache em `<FilesMatch>`.
- PHP: `include`/`require`/`realpath`/`file_exists`/autoload PSR-4 seguem o FS → falham igual ao Linux em diretório CS. `opcache.validate_timestamps` e `realpath_cache` sem alteração.
- Node/Vite/Next: resolução de módulos segue o FS; o erro `Cannot find module './Header'` aparece localmente, não no CI.
- Composer: `vendor/` fica em diretório CS; autoload com nomes errados falha localmente (desejado). Para quem precisa, `forge site case-sensitive off` por site.

## 5. Bancos de dados
- **MySQL/MariaDB**: `lower_case_table_names=0` (paridade Linux) é tentado quando o `data/` da instância está em diretório CS. O servidor testa a sensibilidade do FS no boot; com a flag NTFS o teste passa. **Validar em M1-E1-T2** (manifesto `mysql`/`mariadb`); se o servidor recusar em alguma versão, fallback para `=2` com aviso claro e explicação da diferença. Nunca mudamos esse valor em instância já inicializada (quebra o data dir); a escolha acontece na criação.
- **PostgreSQL**: já é sensível (identificadores sem aspas são rebaixados para minúsculas, como no Linux). Nada a fazer.
- **MongoDB/Redis-compatíveis**: nomes de coleções/chaves já são sensíveis.

### Validação empírica (M1-E1-T2, 2026-10-26)
Script: `scripts/validate-mysql-cs.ps1` (validação manual, não faz parte do produto). Baixa o zip oficial (sha256 conferido), extrai, cria um data dir **vazio**, marca-o case-sensitive via `NtSetInformationFile` (o mesmo caminho do `forge-core`; ver a permissão exigida em §3), gera um `my.ini` equivalente ao de `crates/forge-render/templates/<id>/my.ini.j2`, inicializa, sobe o servidor numa porta livre (a partir de 33070; a 33060 é a porta padrão do protocolo X do MySQL e costuma estar ocupada), conecta pelo cliente da própria distribuição e cria as tabelas `Users` e `users` no mesmo schema. Ambiente: Windows 10 22H2 (19045), NTFS, sem elevação.

| Servidor | Build | `lower_case_table_names` pedido | Efetivo (`@@lower_case_table_names`) | `Users` e `users` coexistem | Resultado |
|---|---|---|---|---|---|
| MySQL | 8.4.13 (`mysql-8.4.13-winx64.zip`, sha256 `e9cd98b1...3ea54`) | 0 | 0 | sim | **PASS** |
| MariaDB | 11.8.9 (`mariadb-11.8.9-winx64.zip`, sha256 `830c4672...963f5`) | 0 | 0 | sim | **PASS** |

Observações:
- **Os dois iniciam e inicializam com `=0` em data dir NTFS case-sensitive**; nenhum fallback foi necessário para o caso padrão. O template `my.ini.j2` (MySQL e MariaDB) emite `lower_case_table_names={{ 0 if instance.case_sensitive else 2 }}`: `0` quando o data dir tem a flag, `2` caso contrário (volume sem suporte, usuário recusou converter).
- **Controle negativo (data dir sem a flag):** com `=0` o MySQL aborta já no `--initialize-insecure` (`MY-010158`: "configured to use case sensitive table names but the data directory is on a case-insensitive file system") e o MariaDB aborta ao subir ("data directory resides on a case-insensitive file system"). Logo `=0` **só** é seguro com a flag; a condição do template é obrigatória, não cosmética. Com `=2` ambos funcionam (efetivo 2, uma única tabela: `CREATE TABLE users` falha com `1050 Table 'users' already exists`).
- **`=2` em data dir case-sensitive** é ignorado pelos dois servidores: ambos detectam o FS e forçam `0` (efetivo 0, as duas tabelas coexistem). Ou seja, o fallback `=2` só tem efeito onde o FS é insensitive.
- A inicialização do MariaDB usa `mariadb-install-db.exe --datadir=... --port=...` (não aceita `--defaults-file`); ele grava um `my.ini` dentro do data dir, ignorado porque o servidor sobe com `--defaults-file` apontando para `etc/mariadb/<instância>/my.ini`. O `lower_case_table_names=0` do `my.ini` passou a valer na primeira subida e o resultado foi PASS.
- MySQL 8.4 sobe com `mysqlx=OFF`, `named-pipe=OFF`, `shared-memory=OFF` no template (sem X protocol, sem pipe, sem memória compartilhada); o servidor aceitou as três opções.
- Não coberto: nunca mudar o valor em instância já inicializada (continua valendo); versões diferentes de 8.4.13/11.8.9 (MySQL 9.x, MariaDB 10.11/11.4/12.3) e Windows 11.

## 6. Case Mismatch Finder (diagnóstico)
Ferramenta no Diagnóstico do site e `forge site lint-case loja.test`: varre o código em busca de referências a arquivos (`require/include/__DIR__` em PHP, `import/require` em JS/TS, `src=`/`href=` em HTML/Blade/Twig, `asset()`/`mix()`/`vite()` do Laravel, `@import`/`url()` em CSS, caminhos em `composer.json`/`package.json`) e compara com o nome real em disco, ignorando o FS: reporta "`Views/header.php` referenciado em `app/Http/Controllers/HomeController.php:14`, arquivo real `views/Header.php`". Oferece **Corrigir** (renomear arquivo ou editar referência) com preview. Também checa classes PSR-4 vs caminho e nomes de tabela em migrations vs. uso em queries (heurístico). Útil mesmo em diretórios insensitive e para projetos herdados.

## 7. Outras paridades Linux ativadas por padrão (desativáveis individualmente)
| Item | Padrão Forgebox | Motivo |
|---|---|---|
| Fim de linha em arquivos gerados pelo Forgebox | LF | igual Linux; Git limpo |
| Encoding | UTF-8 sem BOM | PHP quebra com BOM antes de `<?php` |
| Separador de caminho em configs geradas | `/` | Apache/Nginx/PHP aceitam; igual Linux |
| `.editorconfig` em `forge new` | `end_of_line = lf`, `charset = utf-8` | consistência da equipe |
| Git em `forge new` / `forge site add` (se repo) | `core.ignorecase=false`, `core.autocrlf=false` | Git não esconder renomes de caixa |
| `.gitattributes` sugerido | `* text=auto eol=lf` | |
| Nomes de host | sempre minúsculos (DNS é insensitive; evitamos `Loja.test` vs `loja.test`) | |
| Variáveis de ambiente | **sem paridade possível** (Windows é insensitive); Diagnóstico avisa se `.env` tiver chaves que diferem só por caixa | honestidade |
| Nomes de arquivo reservados/ilegais no Windows (`aux`, `con`, `nul`, `:` `?` `*`) | Finder avisa quando o projeto contém/gera nomes que falhariam no Windows (direção inversa, útil para quem clona de Linux) | |
| Permissões `chmod`/`umask` | sem paridade; Finder avisa para scripts sem shebang/CRLF | |
| Timezone | `date.timezone` definido explicitamente no php.ini (UTC por padrão, editável) | evita warnings e diferenças |
| Locale | `LC_ALL`/`intl` default `C.UTF-8`-like via `setlocale` em `conf.d` | |
| Limites | `memory_limit`, `upload_max_filesize` iguais entre CLI e web | surpresas a menos |

## 8. UI
- Onboarding, passo 3: toggle **"Modo paridade Linux (case-sensitive)"** ligado por padrão, com uma frase: "Seus projetos vão se comportar como no servidor. Erros de maiúsculas/minúsculas aparecem aqui, não em produção."
- Configurações → Sistema de arquivos: toggle global, lista de diretórios e seu estado, botão Converter, link para o Finder.
- Badge por site, aviso no Diagnóstico, resultado do Finder com ação Corrigir.
- CLI: `forge fs case-sensitive status|enable <dir>|disable <dir>|convert <dir>`, `forge site case-sensitive loja.test on|off`, `forge site lint-case loja.test [--fix]`.

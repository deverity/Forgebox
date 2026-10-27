# 15 — Paridade com Linux: case-sensitive por padrão

## 1. Problema
Windows é case-insensitive no sistema de arquivos. Um projeto funciona no Laragon com `require 'Views/header.php'` apontando para `views/Header.php`, uma classe `App\Models\User` em `app/models/user.php`, uma imagem `Logo.PNG` referenciada como `logo.png`, e **quebra ao publicar em Linux**. O desenvolvedor só descobre em produção. O mesmo vale para nomes de tabelas MySQL, rotas, assets e `require` do Node.

## 2. Decisão
**Forgebox é case-sensitive por padrão em tudo que controla**, e o usuário pode desativar globalmente ou por site em Configurações → Sistema de arquivos → "Modo case-sensitive (paridade Linux)". Ver [ADR-0010](adr/0010-case-sensitive-by-default.md).

## 3. Mecanismo: case sensitivity por diretório do NTFS
Desde o Windows 10 1803 o NTFS suporta a flag **por diretório** `FILE_CS_FLAG_CASE_SENSITIVE_DIR`, a mesma usada pelo WSL. Dentro de um diretório marcado, `Header.php` e `header.php` são arquivos distintos e `open("header.php")` falha se só existir `Header.php`, exatamente como no Linux. Novos subdiretórios **herdam** a flag.

- API: `NtSetInformationFile(handle, FileCaseSensitiveInformation { Flags: FILE_CS_FLAG_CASE_SENSITIVE_DIR })` (via `windows` crate / `ntdll`). Equivalente de linha de comando: `fsutil file setCaseSensitiveInfo <dir> enable`.
- Pré-condições: NTFS (não FAT/exFAT/ReFS), Windows 10 1803+, permissão de escrita de atributos no diretório. Se o sistema exigir elevação em alguma edição/política, a operação é roteada pelo `forgebox-helper` (op `fs.set_case_sensitive {path}` validada para estar dentro de parked dirs/raiz do Forgebox, nunca a própria raiz, `bin/` ou `state/`; `forge_core::helper::HelperOp`, [ADR-0022](adr/0022-elevated-helper-protocol.md)).
- Comportamento medido no Windows 10 22H2 (19045), NTFS, sem elevação (M0-E1-T4): a flag **pode** ser ligada em diretório não vazio, mas vale **só para o próprio diretório** (não é recursiva): subdiretórios já existentes mantêm a flag que tinham e só os criados depois herdam. Desligar falha com `STATUS_CASE_DIFFERING_NAMES_IN_DIR` enquanto houver nomes que diferem só por caixa. **Permissão exigida (medido em M1-E1-T2):** `NtSetInformationFile` com `FileCaseSensitiveInformation` falha com `STATUS_ACCESS_DENIED` (0xC0000022) se o usuário não tiver `FILE_DELETE_CHILD` ("Excluir subpastas e arquivos") no diretório, mesmo tendo `FILE_WRITE_ATTRIBUTES`. Diretórios sob `%TEMP%` (dono com Controle Total) funcionam; diretórios criados sob uma raiz cuja ACL herdada dá só *Modify* ao usuário (ex. `C:\apps_src\...` neste ambiente) falham. `fsutil file setCaseSensitiveInfo` exige elevação. Consequência: `ForgeRoot::init()` em uma raiz assim deve conceder `(DC)` ao usuário no diretório recém-criado (`icacls <dir> /grant <user>:(OI)(CI)(DC)`) antes de marcar, ou rotear por `fs.set_case_sensitive` do helper (`needs_elevation`). Por isso o Forgebox marca diretórios **vazios, antes de criar conteúdo** (`ForgeRoot::init()`), e a conversão de diretórios existentes (§3.3) **recria a árvore de pastas** dentro de um diretório novo já marcado e move para lá os arquivos: uma pasta **movida** mantém a flag que tinha (só pastas **criadas** herdam), então mover apenas as entradas do topo deixaria cada projeto insensitive ([ADR-0026](adr/0026-case-conversion-tree-recreation-and-mismatch-finder.md)).
- Implementação em `forge-core::platform::windows::WindowsCaseSensitiveFs` (`NtQueryInformationFile`/`NtSetInformationFile`, `FileCaseSensitiveInformation` = classe 71) atrás do trait `platform::CaseSensitiveFs { support(dir) / is_supported(dir), get(dir), set(dir, bool) }`. Erros tipados: `unsupported_platform` (volume/SO sem suporte, nunca pânico), `needs_elevation` (acesso negado → helper), `conflict` (nomes que diferem só por caixa). Em Linux/macOS (port futuro): `get` devolve o estado real do FS; `set` é no-op; até lá o stub responde `unsupported_platform`. Para o diagnóstico, `info(dir)` devolve o sistema de arquivos do volume (`GetVolumeInformationByHandleW`) e o build do Windows (registro `CurrentBuildNumber`, independente do manifesto do executável); `forge_core::fs::case::status(dir)` (`forge fs case-sensitive status`, `fs_parity.case_status`) junta `supported`, `enabled`, `fs_type`, `os_build`, `win_version_ok` (build ≥ 17134, Windows 10 1803) e `reason`. `capture_dir_meta`/`apply_dir_meta` copiam atributos e datas de uma pasta para a pasta recriada pela conversão (§3.3).

### 3.1 Onde aplicamos
| Diretório | Quando | Como |
|---|---|---|
| `www/` (parked padrão) e qualquer parked dir criado pelo Forgebox | Onboarding (criado vazio → flag antes de qualquer arquivo) | direto |
| Parked dir **existente** com conteúdo (ex. `C:\laragon\www` importado) | `forge park` / import | Oferece **Converter** (§3.3): cria `<dir>.cs-tmp` com a flag, recria as pastas dentro dele e move cada arquivo (rename no mesmo volume, só metadados), troca os nomes, verifica. Reversível. Arquivos abertos abortam e restauram |
| Projeto linkado fora de parked dir (`forge site add D:\x\proj`) | Ao criar o site | Diagnóstico mostra "diretório case-insensitive"; botão **Converter** igual acima; pode recusar |
| `www/<novo projeto>` via `forge new` | Herdado automaticamente | — |
| `data/` dos bancos | Na criação da instância (vazio) | direto (ver §5) |
| `etc/`, `logs/`, `tmp/`, `backups/` | Onboarding (`ForgeRoot::init()`, que também marca as subpastas que cria: `etc/templates`, `etc/sites`, `tmp/downloads`, `tmp/stage`) | direto, por consistência |
| `bin/` | **Não** | Tools Windows esperam FS normal; sem benefício. A extração (`forge_provider::extract`) dá ao staging (`tmp/stage/<id>`, que herdaria a flag de `tmp/`) a sensibilidade do pai do destino antes de escrever, porque a flag viaja com o diretório no rename final |

Em cada site a UI mostra badge **`Aa` case-sensitive** ou **`aA` insensitive** com tooltip explicando. O estado é re-verificado no boot (`fs.case_sensitivity_applied|unavailable`).

**Estado por site (`forge_sites::case_parity`, M1-E4-T6).** O estado de um site é o da **pasta do projeto**: a mais próxima, partindo da raiz do documento (`loja/public`) e subindo até 2 níveis, que tenha `composer.json`, `package.json`, `.git`, `forgebox.yaml` ou `artisan` (sem marcador, a própria raiz). `Aa` só quando ela **e** todas as subpastas olhadas (2 níveis, no máximo 2000 pastas; `forge fs case-sensitive enable` marca só uma pasta, o que dá `partial`) têm a flag. Fica em `sites.case_sensitive` (`SiteDto.case_sensitive`). No boot, `CaseParity::reverify` confere todos os sites com pasta, atualiza a coluna que mudou (`site.updated`) e publica **um** evento por site: `fs.case_sensitivity_applied {dir, site_id}` ou `fs.case_sensitivity_unavailable {dir, site_id, reason, reason_code}`, com `reason_code` estável `unsupported` (volume/SO), `disabled` (revertido), `insensitive` (nunca convertido), `partial`, `missing` (pasta sumiu) ou `error` (leitura falhou/estourou o prazo; a coluna não muda). `forge site case-sensitive <site> on|off` é um job que converte a pasta do projeto (§3.3) e grava a coluna.

### 3.2 Quando não é possível
Volume não-NTFS, Windows < 1803, política de grupo bloqueando, diretório não vazio e usuário recusou converter: o site fica em modo insensitive, com badge e um aviso **não intrusivo** no Diagnóstico. O **Case Mismatch Finder** (§6) continua disponível como rede de segurança.

### 3.3 Conversão de pasta existente (`forge_core::fs::case`, M1-E4-T6)
`forge fs case-sensitive convert <dir> [--revert]` (`fs_parity.case_convert`, job) e o job por site usam `convert_tree`; o mesmo algoritmo liga e reverte. Passos e o que fica no disco se o processo morrer entre eles:

| Passo | No disco depois | `recover()` faz |
|---|---|---|
| 1. Varredura: pastas, arquivos, links, bytes, flag de cada pasta, colisões de caixa (só ao reverter), 20 amostras ≤ 64 MiB com SHA-256 | `dir` | nada |
| 2. Cria `<dir>.cs-tmp` e aplica a flag pedida nele | `dir`, tmp vazio | move de volta, apaga o tmp |
| 3. Recria cada pasta dentro do tmp (herda a flag; conferida) e renomeia cada arquivo e link para lá | `dir` esvaziando, tmp enchendo | idem |
| 4. Troca: `dir` → `<dir>.cs-old`, tmp → `dir` | esqueleto `.cs-old`, `dir` | remove o esqueleto vazio |
| 5. Verifica contagens, bytes, flag em **todas** as pastas e as amostras | idem | idem |
| 6. Remove o esqueleto `.cs-old` (só pastas vazias, `remove_dir`) | `dir` | nada |

- **Falhas** nos passos 2–5 desfazem tudo movendo as entradas de volta: arquivo aberto sem `FILE_SHARE_DELETE` (`conflict` "em uso por outro processo"), pasta que é diretório corrente de algum processo (a troca falha), cancelamento do job (`cancelled`), verificação (`verification_failed`). Nada é apagado além de pastas **vazias**; um arquivo escrito no `.cs-old` durante a conversão faz a pasta ficar e aparecer em `leftover`. `needs_elevation` no passo 2 significa rotear `fs.set_case_sensitive` do tmp pelo helper e repetir.
- **Reverter** é recusado de antemão (`conflict`, com exemplos) quando alguma pasta tem nomes que diferem só por caixa (`A.txt` + `a.txt` colidiriam); toda renomeação recusa substituir uma entrada existente.
- **Já no estado pedido** (todas as pastas): nada é movido (`already`). **Sobras** de uma conversão interrompida são reconhecidas pelas irmãs `.cs-tmp`/`.cs-old` e restauradas por `recover()`, chamado no início de toda conversão (`recovered`).
- **Preservado**: conteúdo, atributos, datas, ACL e identidade de cada arquivo (é um rename); links (junction, symlink, outros reparse points) são movidos inteiros e nunca seguidos; pastas recriadas recebem os atributos (oculto, somente leitura, sistema) e as datas da original. **Não preservado**: ACL **explícita** de pastas (as novas herdam a do pai) e a flag de conteúdo atrás de links.
- Uma conversão por pasta por vez (`conflict` para a segunda); o job reporta fases e `n/total` em `job.progress`, roda em `spawn_blocking`, sem timeout (a duração cresce com a árvore) mas cancelável. Ao terminar: `fs.case_sensitivity_applied` (ligou) ou `fs.case_sensitivity_unavailable` com `reason_code: disabled` (reverteu) ou `unsupported`.
- **Obrigações do daemon**: parar os processos do site antes; quando a pasta convertida **é** um parked dir, parar o `ParkedWatcher` antes e reiniciá-lo depois (o handle do watch acompanha a pasta renomeada). O scan de parked ignora `*.cs-tmp` e `*.cs-old`.
- **Medido** (Windows 10 22H2 19045, NTFS, build de debug, cópia de 3 projetos de `C:\laragon\www` em `%TEMP%`: Laravel com `vendor/`, Vue com `node_modules/` e um site PHP): 34.907 arquivos, 6.783 pastas, 311 MiB convertidos em 21 s e revertidos em 20 s, contagens/bytes/20 hashes iguais, `A.txt` e `a.txt` convivendo na raiz e na pasta mais funda (`crates/forge-core/tests/case_convert.rs`, `laragon_www_copy_roundtrip`, `#[ignore]`).

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

**Implementação (`forge_sites::lint_case`, M1-E4-T6; [ADR-0026](adr/0026-case-conversion-tree-recreation-and-mismatch-finder.md)).**
- **Resolução ignorando o FS**: cada componente do caminho escrito é procurado na listagem **exata** da pasta (cache por varredura); o resultado é o mesmo em pasta sensitive ou insensitive. Reporta só quando nenhuma base/grafia resolve exatamente **e** alguma resolve ignorando a caixa (comparação pelo mapeamento simples para maiúsculas, como a tabela do NTFS): caminhos dinâmicos, rotas e arquivos gerados no build não viram ruído.
- **O que é reconhecido** (expressões regulares; tabela completa na doc do módulo): `require`/`include`(`_once`) com literal, `__DIR__ . '...'` e `dirname(__FILE__) . '...'` (`php-include`); `base_path()`/`app_path()`/`resource_path()`/`public_path()`/`storage_path()`/`database_path()`/`config_path()`/`lang_path()` (`php-path`); `view()`, `View::make()`, `->view()`, `->markdown()` e `@extends`/`@include`/`@includeIf`/`@component`/`@each` com pontos → pastas em `resources/views` e `.blade.php`/`.php` (`php-view`, `blade-include`); `asset()`/`secure_asset()`/`mix()` (raiz do documento), `Vite::asset()`/`@vite([...])`/`vite()` (raiz do projeto) (`asset`); `src=`/`href=`/`data-src=`/`poster=` (`html`); `{% include/extends/embed/import/from/use %}` e `include()`/`source()` do Twig em `templates/` (`twig-include`); `import`/`export ... from`, `import '...'`, `import()`, `require()` relativos e o alias `@/` (→ `resources/js`, `src`) com a resolução do Node/Vite (extensões `.js .jsx .ts .tsx .mjs .cjs .vue .json .svelte .mts` e `index.*`) (`js-import`); `url()`, `@import`, `@use`/`@forward` do Sass com parciais `_nome.scss` (`css`); PSR-4/`classmap`/`files`/`bin` do `composer.json` (`composer`) e `main`/`module`/`types`/`bin`/`files`/`exports` do `package.json` (`package-json`); primeira classe/interface/trait/enum de cada PHP sob uma raiz PSR-4 contra o caminho (`psr4`).
- **Outras checagens** (sem correção automática): nomes que diferem só por caixa na mesma pasta (`case_duplicate`), nomes que o Windows não aceita (`reserved_name`: `con`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`, também com extensão; `<>:"|?*`; controle; ponto/espaço no fim), chaves do `.env` que diferem só por caixa (`env_case_duplicate`) e scripts com shebang terminado em CRLF (`crlf_shebang`).
- **Não percorre** pastas chamadas `vendor`, `node_modules`, `.git`, `storage`, `cache` (e `.svn`, `.hg`); referências **para** elas são verificadas, mas nunca corrigidas renomeando. Arquivos acima de 2 MiB ou que não são UTF-8 são ignorados. Comentários não são removidos.
- **Limites**: só literais (interpolação, concatenação depois do literal, esquema `https:`/`data:`, `{{ }}`/`{% %}`/`<?` e barra invertida são ignorados); sem PSR-0, aliases de `tsconfig` além de `@/`, `~` do webpack, `srcset`; nomes de tabela em migrations vs. queries ficam para depois.
- **Corrigir** (`--fix`): por item, **renomear** o arquivo para a grafia da referência ou **editar** a referência para a grafia do disco. `auto` renomeia quando só o nome do arquivo difere, nada mais o referencia com a grafia atual e todas as referências pedem a mesma grafia (PSR-4 sempre renomeia: a classe manda); senão edita. `--strategy rename|edit` muda o padrão, `overrides` escolhe por id (`arquivo:linha:coluna`), `only` limita. Pedidos conflitantes de renomear o mesmo arquivo viram edição. Ordem: renomeações (componente mais fundo primeiro, em dois passos por um nome temporário, sem substituir entradas), nova varredura, edições (conferem os bytes na posição; gravação atômica preservando BOM e fins de linha), varredura final (`remaining`). `--dry-run` só mostra o plano (`from`/`to` e, para edição, a linha antes/depois).
- **Saída**: JSON estável `{v: 1, root, files_scanned, references_checked, issues: [{id, category, kind, file, line, column, reference, actual, target, expected, ambiguous, detail, fixes, fix, fixed}], fix, dry_run, remaining}` = `CaseLintReportDto` de `fs_parity.lint_case`.
- **Aceite** (`crates/forge-sites/tests/lint_case.rs`, fixture `tests/fixtures/case/`): `base_path('Views/header.php')` vs `views/Header.php` em `HomeController.php:14`, `import Header from './Header'` vs `header.js` em `app.js:1`, `asset('img/Logo.png')` vs `logo.png` em `welcome.blade.php:9`; `--fix` edita a primeira e renomeia os dois arquivos; nova varredura devolve 0; o mesmo numa cópia convertida para case-sensitive, onde as grafias erradas de fato não abrem.

## 7. Outras paridades Linux ativadas por padrão (desativáveis individualmente)
| Item | Padrão Forgebox | Motivo |
|---|---|---|
| Fim de linha em arquivos gerados pelo Forgebox | LF | igual Linux; Git limpo |
| Encoding | UTF-8 sem BOM | PHP quebra com BOM antes de `<?php` |
| Separador de caminho em configs geradas | `/` | Apache/Nginx/PHP aceitam; igual Linux |
| `.editorconfig` em `forge new` | `end_of_line = lf`, `charset = utf-8` | consistência da equipe |
| Git em `forge new` / `forge site add` (se repo) | `core.ignorecase=false`, `core.autocrlf=false` | Git não esconder renomes de caixa |
| `.gitattributes` sugerido | `* text=auto eol=lf` | |

`forge_sites::parity::git_parity_init(dir)` (para `forge new`/`forge site add`, M3/E5) aplica as duas configurações do Git com o `git` da máquina (ou o informado) em processo próprio com timeout, e `git init` antes quando pedido (`forge new`). `.gitattributes` (`* text=auto eol=lf`) e `.editorconfig` (`end_of_line = lf`, `charset = utf-8`) são **criados** quando faltam (LF, UTF-8 sem BOM); arquivos do usuário **nunca** são reescritos: sem a regra de LF, o resultado é uma sugestão do que adicionar no início (anexar no fim sobreporia regras mais específicas nos dois formatos). Pasta sem repositório: configurações do Git puladas (`skipped`).
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
- CLI: `forge fs case-sensitive status|enable <dir>|disable <dir>|convert <dir> [--revert]`, `forge site case-sensitive loja.test on|off`, `forge site lint-case loja.test [--fix] [--dry-run] [--strategy auto|rename|edit]`.

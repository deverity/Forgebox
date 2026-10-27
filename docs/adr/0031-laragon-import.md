# ADR-0031 — Import do Laragon: plano somente leitura, `apply` idempotente, referência por padrão, dry-run pelo mesmo caminho

**Status:** aceito · **Data:** 2026-10-27

## Contexto
[docs/08](../08-release-layout.md) §6 prometia `forge import laragon C:\laragon`: versões de `bin\`, vhosts `auto.*.conf` como sites, `data\` como instâncias, `www\` como parked e preferências do `laragon.ini`, com relatório. A tarefa M1-E5-T5 precisava decidir:
1. Em que crate mora (precisa de drivers de site, descoberta de tools e o store).
2. Como garantir que `--dry-run` decide exatamente o que a importação faria e que o Laragon nunca é escrito.
3. O que fazer com caminhos absolutos gravados nos `.conf` quando a pasta foi movida/copiada.
4. Como uma instância pode apontar para um data dir **fora** de `data/` se o daemon deriva o data dir do manifesto (ADR-0029).
5. Qual PHP os sites importados usam, e o que acontece com o TLD e com nomes que o Forgebox rejeita (`SIMPROS_MATRIZ.test`).

## Decisão
**Mora em `forge-sites::import`.** O importador usa `drivers::detect`/`SiteDraft`/`SiteService` (forge-sites), `forge_registry::discover_in` (forge-registry) e os repositórios do store (forge-core). `forge-sites` já dependia de `forge-core` e `forge-manifest`; ganhou `forge-registry` (que não depende de `forge-sites`, sem ciclo). `forge-provider` não serve: não conhece sites. O daemon só liga as peças (`system.import_laragon`, job): carrega os manifestos do catálogo, fornece `ApplyEnv` (raiz, writer/reader do store, `SiteService`, a configuração atual, e o conversor case-sensitive como closure sobre `CaseConversions::run` do próprio job) e, depois do `apply`, salva `forgebox.toml`, reconcilia as pastas parked, adota as instâncias novas (`ServiceManager::adopt_stored`), cria as instâncias `default` que faltam e re-renderiza os servidores web.

**Dois passos, um caminho.** `LaragonImporter::scan(path) -> ImportPlan` só lê (as únicas execuções são os `version_command` dos manifestos dentro de cada pasta, como o `discover` de sempre) e devolve dados puros (`Serialize`, caminhos com `/`, ordenados: serve de snapshot e de saída). `LaragonImporter::apply(plan, options, env) -> ApplyOutcome` percorre o plano na ordem preferências → tools → instâncias → parked (com a conversão) → sites e, com `dry_run`, para antes de cada gravação (cópia, `rename`, linha no store, site, configuração). Como a decisão (já registrada? destino existe? TLD servido? alias de outro site?) vem do mesmo código, um teste exige que o relatório do `--dry-run` seja igual (`kind`, `name`, `imported`) ao da importação real. Re-executar não cria nada: as linhas existentes nunca são alteradas (`register_if_absent`, checagem de host/instância).

**Relatório estruturado.** `ImportReport { dry_run, items[{kind, name, imported, note}] }` (o `ImportReportDto` do IPC ganhou `dry_run`; no dry-run `imported` = "seria importado"). Tudo que não entra aparece com o motivo (`note`) em pt-BR com chave `import.*` (módulo `messages.rs`, como o resto do crate até existir catálogo de i18n).

**Referência por padrão.** Tools: `tool_versions` com `source: import` apontando para a pasta do Laragon, nome canônico = nome da pasta (sem renomear, docs/08 §5); `--copy-tools` copia para `bin/<tool>/<nome canônico>` por `<nome>.tmp` + `rename`. Só entram versões que o próprio executável respondeu (`Confidence::High`); versão adivinhada pelo nome da pasta ficaria errada para sempre (linhas existentes não são atualizadas) e é listada como pulada. A junction `bin\php\php` (o `switch-php` do Laragon) aparece no discover como uma pasta qualquer: é reconhecida como **link** (`is_reparse_point`) e vira a **versão ativa**, nunca uma versão; o `[php] Version` do `laragon.ini` é o reserva. Qual tool é "o PHP" sai do manifesto (`service.kind: pool`), nunca de um id.

**Data dir de fora da raiz.** `instances.data_dir` já existia, mas o daemon sempre derivava o caminho de `service.daemon.data.dir`. Em vez de tratar todo `data_dir` gravado como verdade (quebraria raiz movida, porque ele é absoluto), a instância importada leva a marca `settings["forgebox.external_data_dir"] = "true"` (`InstanceRow::external_data_dir()`): só com a marca o daemon usa o caminho como está; sem ela o manifesto decide. `services.remove --delete-data` continua só apagando dentro de `data/`. `--move-data` faz `rename` para `data/<tool>/<nome>` (mesma convenção dos manifestos) sem marca; se o destino existir, o volume for outro ou houver arquivo em uso, a pasta fica como referência e o relatório diz por quê; se a linha não puder ser gravada depois do `rename`, a pasta volta.

**Instalação movida.** `laragon.ini` e os `.conf` têm caminhos absolutos de onde o Laragon foi instalado. A raiz original é o pai de `DataDir`; caminhos abaixo dela são lidos da pasta escaneada (`C:/laragon/www/app` → `<escaneada>/www/app`). Com a pasta no lugar de origem nada muda. Isso também permite a fixture dos testes.

**Sites.** `etc/apache2/sites-enabled/auto.*.conf` (`Define`/`${VAR}` expandidos, blocos `:80`/`:443` do mesmo `ServerName` fundidos) e `etc/nginx/sites-enabled/*.conf` (`server {}` com `server_name`/`root`/`listen ... ssl` no nível do `server`). Vhosts que dividem o document root são **um** site: o nome com o TLD do `HostnameFormat` é o preferido e `apply` escolhe como host o primeiro cujo TLD o Forgebox serve (`SiteRules`), os outros viram aliases (`app.local`). `_` vira `-` e maiúsculas viram minúsculas (anotado no relatório); aliases curinga são omitidos. O document root do Laragon é mantido, exceto quando é a própria pasta do projeto: aí vale o root do driver (Laravel `public/`). A série de PHP do site é a do PHP ativo; arquivos do Apache que não são `auto.*` (proxies reversos, `00-default`) são pulados com o motivo em vez de interpretados. Sites explícitos vencem os parked (comportamento do `SiteService::create`).

**Preferências.** `DocumentRootList`/`[apache] DocumentRoot` viram `parked_dirs`. O TLD é **relatado, não aplicado**: DNS, certificados e `SiteRules` nascem do TLD no boot, e trocá-lo silenciosamente em uma importação mudaria o que a máquina resolve; o relatório diz o comando (`forge config set tld`). Hosts de TLD não servido são pulados com esse motivo.

**Case-sensitive.** A pasta de projetos existente com conteúdo é **oferecida** à conversão de [docs/15](../15-linux-parity-case-sensitivity.md) §3.3 (nota no relatório); `--convert-case` roda `CaseConversions::run` dentro do job, **antes** de registrar a pasta como parked (assim não há `ParkedWatcher` com handle na pasta renomeada). Falha na conversão é uma linha do relatório, não aborta a importação.

## Alternativas rejeitadas
- **Importador no daemon:** não seria testável sem processo e duplicaria `SiteDraft`/`discover`; o daemon fica fino.
- **Copiar tools por padrão:** o Laragon tem 5–7 versões de PHP e várias de MySQL (GBs); referência é instantânea, reversível e é o comportamento já documentado de `discover` (docs/08 §5).
- **Tratar todo `data_dir` gravado como verdade:** raiz movida quebraria instâncias gerenciadas (ver acima).
- **Aplicar o TLD do Laragon:** efeitos colaterais amplos (DNS/NRPT, certificados, regras de host) para uma preferência de aparência.
- **Importar `.conf` do Apache que não são `auto.*`:** são configuração livre (proxies, SSL custom); interpretá-los seria adivinhar. O usuário cria `forge proxy add`/`forge site add`.
- **`mklink`/criar `current` das versões importadas:** `current` é do layout engine (docs/08 §2) e as pastas referenciadas ficam fora da raiz; a versão ativa do Laragon só decide a série de PHP dos sites.

## Consequências
- Um data dir inicializado no Laragon (MySQL com `lower_case_table_names=1`) não ganha a paridade Linux de docs/15 §5: o servidor recusa mudar o valor depois de inicializado. Documentado em docs/08 §6.
- O `composer` solto do Laragon (`bin/composer/composer.phar`, sem versão identificável pelo manifesto) e ferramentas sem manifesto ficam listados como pulados.
- Mensagens do relatório são pt-BR; a localização en-US do relatório depende do catálogo de i18n (hoje só o CLI tem).
- `forge_registry::discover_in(bin, manifests)` e `parse_folder_name` passaram a ser públicos.

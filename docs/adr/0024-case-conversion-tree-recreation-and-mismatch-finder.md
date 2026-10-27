# ADR-0024 — Conversão case-sensitive recriando a árvore de pastas; Case Mismatch Finder por listagem exata

**Status:** aceito · **Data:** 2026-10-27

## Contexto
[ADR-0010](0010-case-sensitive-by-default.md) decidiu converter pastas existentes "movendo para um diretório novo já marcado". O comportamento medido do NTFS (Windows 10 22H2, [15 §3](../15-linux-parity-case-sensitivity.md)) mostra um detalhe que a frase esconde:

- A flag `FILE_CS_FLAG_CASE_SENSITIVE_DIR` pertence a **cada diretório**. Um diretório criado dentro de um marcado herda a flag **na criação**; um diretório **movido** (rename) para dentro de um marcado mantém a flag que tinha. Mover só as entradas do topo de `www/` deixaria cada projeto (e tudo abaixo dele) insensitive.
- Ligar a flag em um diretório não vazio funciona neste build, mas só para ele, exige `FILE_DELETE_CHILD` no diretório (falha sob ACL herdada só com *Modify*, como `C:\apps_src`) e versões antigas (1803) só aceitavam diretório vazio (`STATUS_DIRECTORY_NOT_EMPTY`).

O Case Mismatch Finder ([15 §6](../15-linux-parity-case-sensitivity.md)) precisa achar referências com caixa errada **ignorando o FS** (em pasta insensitive, `file_exists('Views/header.php')` é verdadeiro) em PHP, Blade, Twig, HTML, JS/TS, CSS e manifestos, e corrigir com prévia.

## Decisão
**Conversão (`forge_core::fs::case`)** — uma única operação para ligar e para reverter:
1. **Varredura**: conta pastas, arquivos, links e bytes; estado da flag de cada pasta (tudo já no estado pedido → nada a fazer); para reverter, nomes que diferem só por caixa na mesma pasta → `conflict` antes de mexer em qualquer coisa; amostra (reservatório determinístico) de 20 arquivos ≤ 64 MiB com SHA-256.
2. Cria `<dir>.cs-tmp` e aplica a flag pedida nele (`needs_elevation` aqui significa rotear `fs.set_case_sensitive` pelo helper e tentar de novo).
3. **Recria a árvore** dentro do tmp (cada pasta nova herda a flag; é conferida e ajustada se preciso) e **renomeia** cada arquivo e cada link (symlink, junction ou outro reparse point: movido inteiro, nunca seguido). Rename no mesmo volume: só metadados, conteúdo, ACL, atributos e identidade dos arquivos intactos. Atributos (oculto, somente leitura, sistema) e datas das pastas são copiados (`CaseSensitiveFs::capture_dir_meta`/`apply_dir_meta`).
4. **Troca**: `dir` → `<dir>.cs-old` (esqueleto vazio), tmp → `dir`.
5. **Verificação**: mesmas contagens e bytes, todas as pastas com a flag, hashes das amostras iguais.
6. Remove o esqueleto com `remove_dir` (só pasta vazia; arquivo escrito por outro processo durante a conversão faz o `.cs-old` ficar, reportado em `leftover`). Nunca há remoção recursiva.

Qualquer falha nos passos 2–5 (arquivo aberto sem `FILE_SHARE_DELETE`, pasta que é diretório corrente de um processo, cancelamento, verificação) **desfaz** tudo movendo as entradas de volta. Se o processo morrer no meio, `recover()` reconhece o estado pelas irmãs `.cs-tmp`/`.cs-old` e restaura (ou só limpa o esqueleto, se a troca já aconteceu); a próxima conversão chama `recover()` antes. No máximo uma conversão por pasta no processo (trava folha `fs.case_conversions`). O job não tem timeout (a duração cresce com a árvore e o trabalho não pode ser abandonado no meio); é cancelável e o cancelamento restaura.

**Estado por site (`forge_sites::case_parity`)**: é o estado da **pasta do projeto** (a mais próxima, a partir da raiz do documento, com `composer.json`/`package.json`/`.git`/`forgebox.yaml`/`artisan`, até 2 níveis acima): `Aa` só quando ela e todas as subpastas olhadas (2 níveis, até 2000) têm a flag. Guardado em `sites.case_sensitive`, re-verificado no boot com um `fs.case_sensitivity_applied|unavailable` por site; o payload ganhou `reason_code` (`unsupported`, `disabled`, `insensitive`, `partial`, `missing`, `error`).

**Case Mismatch Finder (`forge_sites::lint_case`)**: referências extraídas por expressões regulares por tipo de arquivo e resolvidas contra **listagens exatas** das pastas (cache por varredura), com as regras de resolução de cada ecossistema (extensões e `index.*` do Node, parciais do Sass, `.blade.php`, raiz do documento para `asset()`/`/x`, pastas dos helpers do Laravel, PSR-4 do `composer.json`). Só é reportada a referência que **não** resolve exatamente em nenhuma base e resolve ignorando a caixa: caminhos dinâmicos, rotas e arquivos gerados no build não viram ruído. Correção por item: **renomear** o arquivo para a grafia da referência ou **editar** a referência para a grafia do disco; `auto` renomeia só quando a diferença está no nome do arquivo, nada mais o referencia com a grafia atual e todas as referências pedem a mesma grafia (PSR-4 sempre renomeia: a classe manda); senão edita. Renomeações primeiro (componente mais fundo primeiro, em dois passos por um nome temporário, sem nunca substituir uma entrada), nova varredura, depois as edições (que conferem os bytes na posição e gravam de forma atômica), e uma varredura final (`remaining`). O JSON do relatório é o contrato `fs_parity.lint_case` (`CaseLintReportDto`), verificado por teste.

**Paridade Git (`forge_sites::parity`)**: `core.ignorecase=false` e `core.autocrlf=false` pelo `git` da máquina (Job Object, timeout); `.gitattributes` e `.editorconfig` só são **criados** quando faltam; existentes sem a regra de LF recebem uma sugestão, nunca uma reescrita (anexar no fim sobreporia regras mais específicas do usuário nos dois formatos).

## Alternativas rejeitadas
- **Ligar a flag no lugar em cada pasta existente** (percorrer e chamar `NtSetInformationFile`): preserva tudo e é mais rápido, mas exige `FILE_DELETE_CHILD` em **cada** pasta (falha sob ACL herdada só com *Modify*; pelo helper seria uma elevação por pasta) e não funciona em builds que exigem pasta vazia. Fica como otimização futura quando as permissões permitirem.
- **Mover só as entradas do topo**: deixa as subárvores insensitive (a flag viaja com a pasta movida).
- **Copiar em vez de renomear**: dobra disco e tempo, troca a identidade dos arquivos (hard links, ACLs).
- **`fsutil file setCaseSensitiveInfo` por pasta**: exige elevação.
- **Parsers completos** (PHP, JS/TS, Twig, CSS) no Finder: dependências pesadas e um parser por linguagem; regex + "só reporta se resolve ignorando caixa" mantém os falsos positivos baixos, com os limites documentados em `lint_case`.
- **Renomear pastas automaticamente**: quebraria outras referências que já usam a grafia do disco; renomear pasta só por escolha explícita do usuário (ou PSR-4).

## Consequências
- Tempo proporcional ao número de entradas (medido: 34.907 arquivos, 6.783 pastas, 311 MiB em 21 s convertendo e 20 s revertendo, build de debug, Windows 10 19045).
- Arquivos e pastas abertos impedem a conversão (erro claro, nada alterado): o daemon para os processos do site antes e reinicia o watcher de parked quando a pasta convertida **é** um parked dir (o handle segue a pasta renomeada). O scan de parked ignora `*.cs-tmp` e `*.cs-old`.
- ACL **explícita** de pastas não é preservada (pastas novas herdam a ACL do pai); arquivos mantêm a sua. Conteúdo atrás de junctions/symlinks não é convertido.
- `CaseConvertDto` perdeu `backup` (não existe backup: a reversão é a mesma operação no sentido contrário) e ganhou `enabled`, `links`, `bytes`, `sampled`, `already`, `recovered`, `leftover`, `duration_ms`; `CaseSensitivityDto` ganhou `fs_type`, `os_build`, `win_version_ok`; `LintCaseParams`/`CaseIssueDto`/`CaseLintReportDto` ganharam estratégia, prévia e categorias.

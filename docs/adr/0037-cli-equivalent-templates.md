# ADR-0037 — Templates do comando `forge` equivalente na tabela de métodos

**Status:** aceito · **Data:** 2026-10-28 · Complementa a [ADR-0015](0015-ipc-protocol.md) (tabela única de métodos) e a [ADR-0030](0030-cli-contract.md) (contrato do `forge`); implementa a regra de paridade de [07 §1.7](../07-ui-ux.md).

## Contexto
A regra 7 da UI ([07 §1.7](../07-ui-ux.md)) pede que toda ação da GUI mostre o comando `forge` equivalente, com "Copiar comando". Havia três desenhos concorrentes: (a) uma função `toCli` escrita à mão por tela (E2), (b) um campo livre `cli_hint` por ação e (c) um **template na tabela de métodos**, que já é a fonte única de nome, papel, `local_only` e comandos atendidos (`MethodInfo::cli`). Os dois primeiros duplicam o conhecimento da CLI em TypeScript e envelhecem em silêncio: nada falha quando um flag é renomeado.

## Decisão
1. **Coluna `cli_template` em `api_methods!`.** Cada linha com `cli` não vazio ganha `=> "template"` depois da string de comandos; linhas sem comando (GUI/protocolo) não têm template e um teste falha nos dois sentidos. `MethodInfo::cli_template: Option<&'static str>` e `MethodInfo::render_cli(&serde_json::Value) -> Option<String>`.
2. **Mini-linguagem renderizada a partir dos params JSON** (módulo `forge_ipc::api::cli_template`, gramática completa abaixo). Os mesmos parâmetros que a GUI manda ao daemon produzem a linha de comando; a GUI nunca monta texto de CLI à mão.
3. **Dois renderizadores, um golden.** O renderizador Rust (`render_cli`) e o TypeScript (`apps/desktop/src/ipc/cli.ts`, M2-E1-T13) leem o template de `methods.ts` (campo `cli_template`, emitido por `cargo xtask gen-ts`) e precisam produzir exatamente as saídas de `crates/forge-ipc/tests/fixtures/cli_templates.json` (`{method, params, expected, note?}`). Entradas com `expected: null` documentam o que **não** tem representação.
4. **Ida e volta com o clap.** `crates/forge-cli/tests/cli_templates_roundtrip.rs` quebra cada `expected` em argumentos (regras do `CommandLineToArgvW`), faz `Cli::try_parse_from`, caminha pela árvore de subcomandos e exige que o comando mapeie de volta, pela coluna `cli`, **para o mesmo método** (desempate pelos flags dados na linha: `forge tool update --check` vence `forge tool update`) e que todo valor esteja ligado a um argumento. A única sobreposição aceita na tabela é `forge jobs watch`, atendido por `jobs.get` e por `events.subscribe`. Renomear um flag ou um subcomando na CLI sem atualizar o template quebra o teste.
5. **Cobertura.** Um teste exige template para toda linha de `METHODS` com `cli` não vazio, válido pela gramática, com a primeira palavra de cada alternativa entre os comandos atendidos, e uma entrada no golden para cada alternativa de cada template.
6. **Ações da GUI.** Ações que mudam estado passam por um registro obrigatório (`defineAction`, M2-E1-T13) com regra de ESLint contra chamadas mutáveis diretas; a cobertura contra `cli-only.yaml` é checada em M2-E4-T5 (toda entrada precisa de motivo). Abrir no editor/navegador/pasta é exceção documentada: mostra o comando do programa subjacente, não um `forge`.

### Gramática
Um template é uma ou mais **alternativas** separadas por ` | ` (espaço, barra, espaço). Uma alternativa pode começar com uma **regra** `[cond,cond]`; vale a primeira cuja regra é verdadeira (ou que não tem regra); sem nenhuma, não há comando (`None`/`null`).

| Condição | Verdadeira quando |
|---|---|
| `campo=valor` | o texto do escalar é `valor` (`with=editor`, `scope=local_machine`) |
| `campo` | o campo é "verdadeiro": presente, não `null`, não `false`, não `""`, `[]` ou `{}` |
| `!campo` | o contrário |

Uma alternativa é uma lista de *tokens* separados por espaço: palavra literal (copiada) ou um único `{...}`; o primeiro token é literal (`forge`).

| Placeholder | Resultado |
|---|---|
| `{campo}` | valor posicional; nada se ausente ou `null` |
| `{campo=--flag}` | `--flag valor`; nada se ausente ou `null` |
| `{campo?--flag}` | `--flag` se o valor é `true`; nada caso contrário |
| `{campo?a:b}` | `a` se `true`, `b` se `false` (um lado pode ser vazio); nada se ausente ou `null` |
| `{lista...}` | um argumento por item; um objeto vira `chave=valor` ordenado por chave; nada se vazio |
| `{a.b}` | qualquer forma aceita caminho pontilhado em objetos aninhados |

Valores são strings, inteiros ou booleanos (`true`/`false`); string vazia conta como ausente; array/objeto fora de `{lista...}` e números não inteiros tornam o comando indisponível. Os parâmetros de segurança (`confirm`, `yes`) e segredos (`password`) **nunca são renderizados**: a CLI pede a confirmação e a senha; o comando copiado não carrega segredo nem pula a pergunta. Regras servem aos métodos que atendem vários comandos: `sites.open` (`with=editor|terminal|explorer`), `shell.env` (`format`), `tls.ca_install` (`scope`), `tls.export` (`format=pfx`), `tools.update`/`tools.check_updates` (com ou sem `tool`).

### Aspas que sobrevivem ao PowerShell e ao `cmd.exe`
`quote_arg` produz o texto que os dois shells entregam sem alteração ao `forge.exe`, que lê a linha pelas regras do `CommandLineToArgvW`:
- só `[A-Za-z0-9_+=:./\@-]` (e não começando em `@`): sem aspas (`C:\www\loja`, `php@8.3`, `mysql/default`);
- qualquer outro caractere (espaço, `&`, `|`, `(`, `)`, `^`, `;`, `,`, `'`, não ASCII): entre aspas duplas, que os dois shells mantêm literais para esses caracteres; uma sequência de `\` no fim é dobrada para não escapar a aspa de fechamento (`"D:\meus sites\\"`). `,` fica entre aspas porque no PowerShell é operador de array; `@` inicial, porque é *splatting*;
- **sem representação** (`None`, em vez de um comando errado): string vazia (o PowerShell 5.1 descarta argumento vazio de programa nativo), `"` (difere entre os shells), `$` e `` ` `` (o PowerShell expande dentro de aspas duplas), `%` (o `cmd.exe` expande `%VAR%`), caracteres de controle e valores que começam com `-` (seriam lidos como flag).

## Alternativas consideradas
- **`toCli` escrito à mão em TypeScript por tela (E2):** simples de começar, mas duplica a CLI, não tem teste que ligue a string ao clap e esquece métodos novos. Descartado.
- **Gerar o comando a partir do clap (`Cli::command()`) em runtime:** o clap não conhece os params da API (`since_secs` vs `--since`, `secure: bool` vs `secure|unsecure`), e a GUI não carrega o `forge-cli`. Descartado.
- **Um campo `cli_hint` livre (texto fixo):** não reflete os valores da ação (o ponto de "Copiar comando"). Descartado.
- **Aspas por shell (`'...'` no PowerShell, `"..."` no `cmd`):** a GUI não sabe qual shell o usuário usa; aspas duplas funcionam nos dois para tudo que não seja `$`, `` ` ``, `%` ou `"`, e esses viram "sem comando" em vez de um comando que muda de significado.
- **Omitir valores padrão** (`--window 5m`, `--scope user`): menos ruído, mas exige uma sintaxe a mais nos dois renderizadores. Fica para uma emenda se o texto incomodar; o comando renderizado já é equivalente.

## Consequências
- Toda linha nova de `METHODS` precisa de `=> "template"` (ou `cli` vazio) e do stub no clap **no mesmo PR**; o teste de cobertura e o de ida e volta apontam o que falta. Os contratos de M2-E1-T3, M2-E2-T1, M2-E3-T1 e M2-E5-T1 seguem isso.
- `methods.ts` ganha `cli_template: string | null` por método (`gen-ts` sem drift é checado na CI); a API não muda (nenhum campo no fio), então `API_VERSION` não sobe.
- O renderizador TypeScript (M2-E1-T13) é validado contra o mesmo golden; uma divergência de gramática aparece como diferença de string, não como comportamento sutil.
- Comandos que dependem de entrada fora dos params (`forge exec -- <comando>`, `forge env --powershell` com `versions`) ficam de fora do template (só o `forge env` de `shell.env`); `forge daemon logs`, `forge link`, `forge unlink` e `forge proxy remove` equivalem a formas já cobertas (`forge logs daemon`, `forge site add`, `forge site remove`).

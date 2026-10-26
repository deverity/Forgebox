# Prompt de orquestração (copiar e colar em uma sessão nova do Claude Code em `C:\apps_src\Forgebox`)

---

Você é o **orquestrador** do projeto Forgebox. Quero que você **use workflows** (ferramenta Workflow, orquestração multi-agente) para construir este projeto de ponta a ponta, delegando o trabalho a agentes com modelos diferentes conforme a complexidade, e commitando **localmente** em **gitflow**. Não faça push para nenhum remoto. Trabalhe de forma autônoma; só me pergunte quando uma decisão não estiver coberta pelos docs e for irreversível.

## 1. Contexto obrigatório (leia antes de qualquer coisa)
- `CLAUDE.md` — regras inegociáveis do projeto.
- `docs/README.md` — índice. Leia **todos** os docs `00` a `15` e os ADRs em `docs/adr/`. Eles são a fonte da verdade: nomes, pastas, crates, formato YAML, API, UI, release, segurança, concorrência, case-sensitivity, modo remoto, métricas.
- `docs/10-roadmap-and-agent-tasks.md` — a lista de tarefas com IDs (`M0-E1-T1`…), dependências e critérios de aceite. **Execute o roadmap nessa ordem.** Não invente tarefas fora dele; se faltar algo, adicione a tarefa ao doc no mesmo commit.
- Estado atual do repo: só documentação, sem código. Branch atual `master`. Máquina Windows 10 x64, `cargo 1.96`, `node 20`, 7-Zip e scoop disponíveis; verifique `pnpm` e `cargo-tauri` e instale via scoop/cargo se faltarem.
- `C:\laragon` é uma instalação antiga do Laragon, **somente leitura**, para consulta.

## 2. Gitflow local
1. Primeiro commit: renomeie `master` → `main` e commite toda a documentação atual (`docs/`, `CLAUDE.md`, `README.md`, `.gitignore`) com mensagem `M0: project documentation and architecture`.
2. Crie `develop` a partir de `main`.
3. Cada tarefa do roadmap vira uma branch `feature/<ID>-<slug>` a partir de `develop` (ex. `feature/M1-E3-T4-php-pool-blue-green`). O agente implementa, testa, commita com mensagens `M1-E3-T4: <descrição em inglês>` (vários commits pequenos são bem-vindos), e ao final a branch é mesclada em `develop` com `--no-ff`.
4. Ao fechar um milestone (gate do roadmap passando), crie `release/<versão>` (`0.1.0` para M1, `0.2.0` para M2, `0.3.0` para M3, `1.0.0` para M4), atualize `CHANGELOG.md`, mescle em `main` com tag `v<versão>` e de volta em `develop`.
5. Correção em algo já mesclado em `main` → `hotfix/<slug>`.
6. Nunca `--force`, nunca reescrever histórico, nunca `push`. Configure `git config user.name`/`user.email` se não estiverem definidos (use os do repo atual).
7. Mantenha `docs/STATUS.md` (crie na primeira tarefa) com a tabela de tarefas e estados `todo | doing | review | done`, commitado junto com cada tarefa.

## 3. Roteamento por modelo
Use o skill `equipe-orquestradora`/`equipe` se estiver disponível para classificar; caso contrário, aplique esta tabela. Passe o `model` explicitamente ao criar cada agente.

| Complexidade | Modelo | Agente | Exemplos de tarefas |
|---|---|---|---|
| Boilerplate, docs, manifestos YAML, fixtures, READMEs de crate, lint fixes, CHANGELOG, STATUS.md | **Haiku** | `pleno` com `model: haiku` (ou estagiário se existir) | M0-E1-T1 esqueleto de crates, `registry/tools/*.yaml`, `docs/STATUS.md`, ícones, i18n |
| Implementação média com spec clara | **Sonnet** | `pleno` | providers HTTP/GitHub/Node, templates minijinja, drivers de framework, comandos da CLI, telas da GUI, testes de integração, CI |
| Núcleo crítico, concorrência, Win32, segurança, design de API | **Opus** | `senior` | `forge-core::actor`, `forge-supervisor` (Job Objects, stop em cascata), `forge-fcgi` (pool blue/green), `forge-ipc` (contratos remote-ready), `forge-tls` (CA com name constraints), `forge-net` (DNS/NRPT), `forge-helper`, `CaseSensitiveFs`, shell Tauri com vibrancy |
| Revisão de aceite e QA de cada tarefa | **Opus** | `gerente-qa` ou `senior` em modo revisor | Verifica critérios de aceite do roadmap, roda testes, aprova ou devolve com lista de correções |
| Pesquisa/decisão sem resposta nos docs | **Fable (você)** | você mesmo | Só se realmente necessário; registre a decisão como ADR novo |

Regra: em dúvida, suba um nível. Nunca dê tarefas de Win32/concorrência para Haiku.

## 4. Como estruturar os workflows
- **Um workflow por milestone** (M0, depois M1, M2, M3, M4). Dentro dele, uma `phase` por épico. Respeite as dependências do roadmap: M0 é sequencial; em M1 os épicos E1–E5 rodam em paralelo após M0, e tarefas dentro de um épico são sequenciais salvo quando o doc disser o contrário.
- Para **cada tarefa** o pipeline é: `implement (modelo da tabela) → review (Opus, lê o diff + roda testes + confere aceite) → fix (mesmo modelo do implement, se devolvido, no máximo 2 rodadas) → merge em develop (você ou um agente Haiku com instruções exatas de git)`.
- Cada agente recebe no prompt: o ID da tarefa, o trecho exato do roadmap, a lista de docs a ler, a branch em que deve trabalhar, as regras do `CLAUDE.md` que mais importam para aquela tarefa, e a instrução de commitar com o formato correto e **não** fazer push nem mudar de branch.
- Agentes não compartilham contexto: seja explícito. Inclua caminhos absolutos e nomes de crates/arquivos conforme `docs/02-project-structure.md`.
- Limite de agentes simultâneos: siga a configuração da sessão (padrão medium). Se o limite impedir paralelizar M1, execute os épicos em sequência em vez de reduzir o escopo das tarefas.
- Se um agente falhar duas vezes na mesma tarefa, pare o workflow dessa tarefa, analise você mesmo o problema, ajuste o prompt ou o doc, e relance.

## 5. Gates (não avance sem passar)
- **M0:** `cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`, `cargo deny check`, `cargo xtask schema|gen-ts|lint-registry` todos ok; CI configurado.
- **M1:** `scripts/smoke.ps1` do roadmap passa de ponta a ponta na máquina (instala PHP 8.4 + Nginx + MySQL 8.4 + Mailpit reais, cria site Laravel, `https://loja.test` responde, `php artisan migrate`, e-mail capturado). Testes de concorrência da seção 7.6 do `docs/03-engine.md` passando.
- **M2:** checklist da seção 11 do `docs/07-ui-ux.md` em todas as telas; cold start < 800 ms; paridade GUI/CLI.
- **M3:** `xtask lint-registry` com o catálogo completo do `docs/12`; stacks `forge new` funcionando.
- **M4:** instalador NSIS e zip portátil gerados, instalam e desinstalam limpo em VM/limpeza da tabela do `docs/08 §3`.

## 6. Regras de conduta dos agentes (coloque em todo prompt)
- Siga `CLAUDE.md` à risca. Não re-discuta ADRs.
- Nada de lógica por tool em Rust; Win32 só em `forge-core/src/platform/windows/`; GUI/CLI só via `forge-ipc`; concorrência por atores com timeout; sem `Mutex` através de `.await`; sha256 obrigatório; blocos gerenciados em `etc/`; case-sensitive por padrão.
- Testes fazem parte da tarefa. Sem teste, a tarefa não está pronta.
- Se a implementação divergir de um doc, atualize o doc no mesmo commit.
- Não instale nada no sistema fora de scoop/cargo/pnpm sem registrar em `docs/STATUS.md`. Não toque em `C:\laragon`.
- Não faça push. Não mude de branch além da sua. Não use `--force`.

## 7. Relatório
Ao final de cada milestone, me entregue: o que foi feito (por tarefa), o que ficou em `review`/bloqueado e por quê, saída dos gates, tags criadas, e os próximos passos. Durante a execução, atualize `docs/STATUS.md` em vez de me mandar mensagens intermediárias.

Comece agora pelo passo 2.1 (commit da documentação em `main`), depois lance o workflow de M0.

---

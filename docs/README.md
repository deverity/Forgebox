# Forgebox — Documentação

> Ambiente de desenvolvimento PHP (e stacks web em geral) local para Windows, escrito em Rust.
> Objetivo: ser melhor que Laragon, Laravel Herd, ServBay, XAMPP e WampServer **em tudo**: função, controle, confiabilidade e UI/UX.

Esta pasta é a fonte da verdade do projeto. Agentes e humanos devem ler na ordem abaixo antes de implementar qualquer coisa.

| # | Documento | O que define |
|---|-----------|--------------|
| 00 | [Visão e análise competitiva](00-vision.md) | Por que existir, o que os concorrentes fazem, onde vamos superá-los, pilares do produto |
| 01 | [Arquitetura](01-architecture.md) | Processos, crates, IPC, fluxo de dados, decisões de stack |
| 02 | [Estrutura do repositório](02-project-structure.md) | Árvore de pastas do código-fonte e responsabilidade de cada uma |
| 03 | [Motor (engine)](03-engine.md) | Supervisor de processos, ciclo de vida de serviços, saúde, portas, eventos, pool FastCGI de PHP |
| 04 | [Especificação do manifesto YAML](04-tool-manifest-spec.md) | O padrão `forgebox.yaml` que acompanha cada tool: download, instalação, serviço, config, UI; kinds Tool/Extension/Stack/Project |
| 05 | [Providers e auto-update](05-providers-and-updates.md) | De onde baixamos cada tool, verificação, políticas de atualização, rollback |
| 06 | [Sites, vhosts, multi-PHP, proxy, DNS e SSL](06-sites-vhosts-multi-php.md) | Modelo de site, PHP por vhost, reverse proxy para Node/Next, processes, `*.test`, CA local, túneis |
| 07 | [UI/UX e design system](07-ui-ux.md) | Feel Apple-ish: tokens, tipografia, telas, interações, motion, tray, atalhos |
| 08 | [Layout do release para o cliente final](08-release-layout.md) | Árvore instalada, instalador, modo portátil, o que toca fora da raiz, desinstalação, import do Laragon |
| 09 | [CLI `forge`](09-cli.md) | Comandos, saída, códigos de saída |
| 10 | [Roadmap e tarefas para agentes](10-roadmap-and-agent-tasks.md) | Milestones, épicos, critérios de aceite, regras para agentes |
| 11 | [Segurança](11-security.md) | Privilégios, helper elevado, cadeia de confiança, CA com name constraints, IPC |
| 12 | [Catálogo completo e compatibilidade de stacks](12-stacks-compat.md) | Todas as tools (runtimes, servidores, bancos, cache, mail, túneis, GUIs), status nativo/WSL, stacks e matriz servidor × runtime |
| 13 | [Modo remoto](13-remote-mode.md) | Controlar um Forgebox em Linux/outro Windows a partir da GUI/CLI: transportes, pareamento mTLS/SSH, perfis, diferenças por SO |
| 14 | [Observabilidade por vhost](14-observability-metrics.md) | CPU, memória, req/s, p50/p90/p99, erros e consultas lentas por projeto; pipeline, UI, CLI, exportação |
| 15 | [Paridade Linux: case-sensitive por padrão](15-linux-parity-case-sensitivity.md) | Flag NTFS por diretório, conversão, servidores/PHP/bancos, Case Mismatch Finder, outras paridades (LF, UTF-8, Git) |
| ADR | [Decisões de arquitetura](adr/README.md) | Registros curtos de cada decisão relevante e alternativas rejeitadas |
| design | [Artefatos de design](design/README.md) | Tokens, mockups, componentes, motion, ícones |

## Convenções rápidas

- **Nome do produto:** Forgebox. **CLI:** `forge`. **Daemon:** `forgeboxd`. **GUI:** `Forgebox.exe`. **Helper elevado:** `forgebox-helper`.
- **Plataforma alvo v1:** Windows 10 (1809+) e Windows 11, x64. A arquitetura não pode impedir macOS/Linux no futuro, mas nada de v1 deve ser bloqueado por isso.
- **Idioma:** documentação em pt-BR; identificadores, código, nomes de arquivo, chaves YAML e mensagens de log em inglês.
- **Raiz instalada:** `C:\Forgebox` por padrão (configurável). Caminhos internos sempre relativos à raiz → o produto é portátil por construção.
- **Instalação sob demanda:** o instalador não embute tools; o catálogo é completo e cada item é baixado quando o usuário clica Instalar.
- **Nunca** exigir Windows Services para rodar a stack. Elevação só para o estritamente necessário (NRPT/hosts, trust store, firewall) e via helper auditável.
- **Toda tool é descrita por um manifesto YAML.** Nada de lógica hardcoded por tool no motor.
- **Case-sensitive por padrão** (paridade Linux) em `www/`, `etc/`, `data/`; desativável global/por site ([15](15-linux-parity-case-sensitivity.md)).
- **Nada bloqueia o controlador:** atores com mailbox, timeouts em tudo, operações longas viram `job_id` + eventos ([03 §7](03-engine.md)).
- **Multi-versão simultâneo** é o padrão: `bin/<tool>/<versão>` + junction `current` para CLI; pools por versão de PHP para web.

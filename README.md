# Forgebox

**A stack local inteira, sob controle total, com um clique.**

Ambiente de desenvolvimento local para Windows (PHP, Node, bancos, cache, mail, túneis) escrito em Rust: motor pequeno e determinístico, múltiplas versões de tudo rodando ao mesmo tempo (cada vhost escolhe seu PHP), catálogo aberto de ferramentas em YAML com instalação sob demanda e auto-update, DNS `*.test` e HTTPS locais de verdade, e uma interface com acabamento de app Apple.

Pensado para superar Laragon, Laravel Herd, ServBay, XAMPP e WampServer em função, controle e UI/UX.

## Status
Fase de especificação. Toda a arquitetura, o motor, o padrão de manifestos, o layout de release e o roadmap estão documentados em [`docs/`](docs/README.md). Implementação começa pelo [roadmap](docs/10-roadmap-and-agent-tasks.md).

## Documentação
- [Visão e análise competitiva](docs/00-vision.md)
- [Arquitetura](docs/01-architecture.md) · [Estrutura do repositório](docs/02-project-structure.md) · [Motor](docs/03-engine.md)
- [Manifesto YAML de tools](docs/04-tool-manifest-spec.md) · [Providers e auto-update](docs/05-providers-and-updates.md)
- [Sites, multi-PHP, proxy, DNS, SSL](docs/06-sites-vhosts-multi-php.md) · [Catálogo e compatibilidade](docs/12-stacks-compat.md)
- [UI/UX](docs/07-ui-ux.md) · [Layout do release](docs/08-release-layout.md) · [CLI](docs/09-cli.md) · [Segurança](docs/11-security.md)
- [Modo remoto](docs/13-remote-mode.md) · [Observabilidade por vhost](docs/14-observability-metrics.md) · [Paridade Linux / case-sensitive](docs/15-linux-parity-case-sensitivity.md)
- [ADRs](docs/adr/README.md)

## Licença
MIT (núcleo). Ferramentas de terceiros mantêm suas licenças.

# Status do roadmap

Fonte: [10-roadmap-and-agent-tasks.md](10-roadmap-and-agent-tasks.md). Estados: `todo | doing | review | done`.
Ao iniciar uma tarefa, marcar `doing` (primeiro commit); ao terminar, `review` (último commit). Só `done` após revisão aprovada.

| ID | Tarefa | Estado | Branch | Observações |
|---|---|---|---|---|
| M0-E1-T1 | Workspace Cargo | done | feature/M0-E1-T1-workspace | 16 crates + xtask com testes; cargo deny ok (warnings de allowances de licença ainda não usadas, previstas); ADR-0012 (serde_yaml_ng) |
| M0-E1-T2 | xtask | doing | feature/M0-E1-T2-xtask | |
| M0-E1-T3 | CI | todo | — | |
| M0-E1-T4 | forge-core base | todo | — | |
| M0-E1-T5 | Esqueleto de concorrência | todo | — | |
| M1-E1-T1 | Schema v1 (manifesto) | todo | — | |
| M1-E1-T2 | Manifestos iniciais | todo | — | |
| M1-E1-T3 | Registry loader | todo | — | |
| M1-E1-T4 | Discover | todo | — | |
| M1-E2-T1 | Download engine | todo | — | |
| M1-E2-T2 | Verify + extract | todo | — | |
| M1-E2-T3 | Providers | todo | — | |
| M1-E2-T4 | Layout + current | todo | — | |
| M1-E2-T5 | Update engine | todo | — | |
| M1-E3-T1 | Job Objects + spawn + stop em cascata | todo | — | |
| M1-E3-T2 | Health probes + estados + restart + métricas | todo | — | |
| M1-E3-T3 | Pré-checagens | todo | — | |
| M1-E3-T4 | PhpPool | todo | — | |
| M1-E4-T1 | Templates de servidor web | todo | — | |
| M1-E4-T2 | Drivers de framework | todo | — | |
| M1-E4-T3 | DNS server + NRPT | todo | — | |
| M1-E4-T4 | TLS | todo | — | |
| M1-E4-T5 | Proxies + processes | todo | — | |
| M1-E4-T6 | Paridade Linux | todo | — | |
| M1-E5-T1 | Contratos IPC | todo | — | |
| M1-E5-T2 | Daemon | todo | — | |
| M1-E5-T3 | CLI completa | todo | — | |
| M1-E5-T4 | Helper elevado | todo | — | |
| M1-E5-T5 | Import do Laragon | todo | — | |
| M2-E1 | Shell do app (Tauri) | todo | — | |
| M2-E2 | Telas núcleo | todo | — | |
| M2-E3 | Telas complementares | todo | — | |
| M2-E4 | Design QA | todo | — | |
| M2-E5 | Métricas v1 | todo | — | |
| M3-E1 | Catálogo completo de manifestos | todo | — | |
| M3-E2 | Stacks + forge new + drivers restantes | todo | — | |
| M3-E3 | forge share, LAN, backups, bundles | todo | — | |
| M4-E1 | Instalador NSIS, portátil, desinstalador | todo | — | |
| M4-E2 | Documentação do usuário | todo | — | |
| M4-E3 | Beta fechado | todo | — | |

## Ferramentas instaladas na máquina de dev

| Ferramenta | Versão | Como foi instalada | Data |
|---|---|---|---|
| pnpm | 10.7.1 | scoop | 2026-10-26 |
| cargo-deny | 0.20.2 | `cargo install` | 2026-10-26 |

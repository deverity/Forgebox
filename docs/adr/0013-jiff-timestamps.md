# ADR-0013 — Timestamps UTC com `jiff::Timestamp`

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Eventos (`Event.ts`), linhas do SQLite (`created_at`, `applied_at`...), histórico de updates e, em breve, contratos da API precisam de um tipo único de instante UTC, serializável como texto legível (RFC 3339) e exportável para TypeScript pelo `cargo xtask gen-ts`. O workspace ainda não tinha crate de tempo; a escolha vale para todos os crates, então é feita uma vez (M0-E1-T4).

## Decisão
Usar **`jiff`** (`default-features = false`, features `std` + `serde`) e expor o tipo como `forge_core::Timestamp` (re-export de `jiff::Timestamp`). Regras:
- Instantes são sempre UTC (`Timestamp`); no fio e no SQLite viram texto RFC 3339 (`2026-10-26T13:00:00.123Z`). Ordenação de eventos é por `seq`, nunca pelo texto do timestamp.
- Sem tzdb embutido. Quando houver necessidade de fuso (ex. `quiet_hours` em hora local), habilitar só a feature `tz-system` de jiff (lê o fuso do SO), registrando no PR.
- `specta` exporta `Timestamp` como `string` (feature `jiff` do specta, ligada pela feature `specta` do forge-core).

## Alternativas rejeitadas
- **`chrono`:** API mais antiga, histórico de soundness em fuso local, tipos `DateTime<Utc>` mais verbosos; sem vantagem para o nosso uso.
- **`time`:** bom, mas formatação/parse RFC 3339 exigem features e macros extras; integração com specta equivalente.
- **Inteiro (ms desde a época):** compacto, mas ilegível em logs/SQLite e propenso a confundir unidades; perde o tipo.

## Consequências
- Licença `Unlicense OR MIT` (aceita pelo `deny.toml` via MIT).
- Crates de domínio usam `forge_core::Timestamp` em vez de declarar `jiff` diretamente (a menos que precisem de `jiff::civil`/`Span`).
- Trocar de crate no futuro exige novo ADR e mexe no formato só se o texto RFC 3339 mudar (não deve).

// Loads every `src/features/<name>/slots.ts` once, so a feature registers its site tabs,
// dashboard sections, Atenção sources and settings sections without anyone editing the shell.
// Those modules stay tiny (they register `lazy()` components); the screens themselves are
// lazy route chunks.
export const slotModules = import.meta.glob<unknown>("../features/*/slots.ts", { eager: true });

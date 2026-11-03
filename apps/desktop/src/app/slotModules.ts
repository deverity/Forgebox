// Loads every `src/features/<name>/slots.ts` once, so a feature registers its site tabs,
// dashboard sections, Atenção sources and settings sections without anyone editing the shell.
// Those modules stay tiny (they register `lazy()` components); the screens themselves are
// lazy route chunks.
// Untyped call on purpose: Vitest's fsModuleCache only skips modules whose source contains the
// literal `import.meta.glob(`; `glob<T>(` would cache a stale feature list (src/test/globCache.test.ts).
export const slotModules: Record<string, unknown> = import.meta.glob("../features/*/slots.ts", {
  eager: true,
});

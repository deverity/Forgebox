// Database actions (docs/07 §5.6, ADR-0040). `db.export` / `db.import` are jobs; `db.shell`
// and `db.url` do not mutate but are registered too so the screen shows their `forge` command
// and runs them through the same queue.
import { defineAction } from "./registry";

/** Creates a database (`forge db create loja --on mysql/default --charset utf8mb4`). */
export const createDatabase = defineAction<
  "db.create",
  { instance: string; name: string; charset: string | null }
>({
  id: "db.create",
  method: "db.create",
  toParams: ({ instance, name, charset }) => ({
    name,
    instance,
    ...(charset === null ? {} : { charset }),
  }),
});

/** Drops a database; the daemon refuses it without `confirm`, which the typed name stands for. */
export const dropDatabase = defineAction<"db.drop", { instance: string; name: string }>({
  id: "db.drop",
  method: "db.drop",
  toParams: ({ instance, name }) => ({ name, instance, confirm: true }),
});

/** Exports a database to the daemon's default backup path unless `output` is given. */
export const exportDatabase = defineAction<
  "db.export",
  { instance: string; name: string; output?: string | null }
>({
  id: "db.export",
  method: "db.export",
  toParams: ({ instance, name, output }) => ({
    name,
    instance,
    ...(output === undefined || output === null || output === "" ? {} : { output }),
  }),
});

/** Imports a `.sql`, `.sql.gz` or `.sql.zst` file into a database. */
export const importDatabase = defineAction<
  "db.import",
  { instance: string; name: string; file: string }
>({
  id: "db.import",
  method: "db.import",
  toParams: ({ instance, name, file }) => ({ name, instance, file }),
});

/** The command line of the instance's client; the GUI opens it in an integrated terminal. */
export const databaseShell = defineAction<
  "db.shell",
  { instance: string; database: string | null }
>({
  id: "db.shell",
  method: "db.shell",
  toParams: ({ instance, database }) => ({
    instance,
    ...(database === null ? {} : { database }),
  }),
});

/** DSN-style URL plus `.env` variables of a database (contains credentials). */
export const databaseUrl = defineAction<"db.url", { instance: string; database: string }>({
  id: "db.url",
  method: "db.url",
  toParams: ({ instance, database }) => ({ instance, database }),
});

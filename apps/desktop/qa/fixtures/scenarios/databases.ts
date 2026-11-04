// Handlers of the Databases screen and the site "Banco" tab (M2-E3-T14): `db.list` with a
// running MySQL and a stopped PostgreSQL, `db.create|drop` that follow the daemon's name rule,
// `db.url` with a password, `db.shell`, and `db.export|import` jobs that report progress. The
// state lives in the closure (one set per scenario); tests build their own with `createDatabasesQa`.
//
// Names with a meaning for QA:
// * `broken_*`  -> the job fails with the last client lines in the message (stderr tail);
// * `hold_*`    -> the job keeps running until `jobs.cancel`;
// * `big_*`     -> a longer export (several progress steps).
import { fakeError, fakeJob, type FakeHandlers } from "../../../src/ipc/fakeBridge";
import type * as T from "../../../src/ipc/generated";
import { fakeId } from "./data";

const NAME_RE = /^[A-Za-z0-9_]{1,64}$/;
const CHARSET_RE = /^[A-Za-z0-9_]{1,32}$/;
const ACK: T.Ack = { ok: true, message: null };
const MIB = 1024 * 1024;
/** Time between the progress steps of a normal export or import. */
const STEP_MS = 120;
const BACKUPS = "C:/Forgebox/backups/mysql/default";

export interface DatabasesQa {
  instances: T.DbInstanceDto[];
  databases: T.DatabaseDto[];
  handlers: FakeHandlers;
}

function dbInstance(n: number, tool: string, version: string, patch: Partial<T.DbInstanceDto>) {
  return {
    instance_id: fakeId("01QAD8IN", n),
    instance: `${tool}/default`,
    tool,
    version,
    protocol: tool === "postgresql" ? "postgresql" : "mysql",
    state: "stopped",
    port: null,
    data_size_bytes: null,
    capabilities: {
      create: true,
      drop: true,
      export: true,
      import: true,
      shell: true,
      url: true,
      open_with: [],
    },
    ...patch,
  } satisfies T.DbInstanceDto;
}

export function qaDatabase(instance: T.DbInstanceDto, name: string, tables: number, bytes: number) {
  return {
    instance_id: instance.instance_id,
    instance: instance.instance,
    name,
    charset: instance.protocol === "postgresql" ? "UTF8" : "utf8mb4",
    collation: instance.protocol === "postgresql" ? "en_US.UTF-8" : "utf8mb4_0900_ai_ci",
    size_bytes: bytes,
    tables,
  } satisfies T.DatabaseDto;
}

/** Fresh QA state; `running: false` starts MySQL stopped, `empty` has no instance at all. */
export function createDatabasesQa(
  options: { running?: boolean; empty?: boolean } = {},
): DatabasesQa {
  const mysql = dbInstance(1, "mysql", "8.4.13", {
    state: options.running === false ? "stopped" : "running",
    port: options.running === false ? null : 3306,
    data_size_bytes: 412 * MIB,
  });
  const postgres = dbInstance(2, "postgresql", "17.11", { state: "stopped" });
  const instances = options.empty === true ? [] : [mysql, postgres];
  const databases = [
    qaDatabase(mysql, "loja", 42, 120 * MIB),
    qaDatabase(mysql, "blog", 12, 8 * MIB),
    qaDatabase(mysql, "Loja", 3, MIB),
  ];
  const find = (ref: string | null | undefined): T.DbInstanceDto => {
    const found =
      instances.find((i) => i.instance === ref) ?? instances.find((i) => i.state === "running");
    if (found === undefined) fakeError("not_found", `database instance ${String(ref)} not found`);
    return found;
  };
  const running = (ref: string | null | undefined): T.DbInstanceDto => {
    const found = find(ref);
    if (found.state !== "running") {
      fakeError("conflict", `${found.instance} is not running`, "instance_not_running", {
        instance: found.instance,
      });
    }
    return found;
  };
  const checkName = (name: string) => {
    if (!NAME_RE.test(name)) {
      fakeError("invalid_input", `database name ${name} is not valid`, "database_name", {
        field: "name",
      });
    }
  };
  const handlers: FakeHandlers = {
    "db.list": (params) => ({
      databases: databases.filter(
        (db) =>
          (params.instance === undefined ||
            params.instance === null ||
            db.instance === params.instance) &&
          instances.find((i) => i.instance === db.instance)?.state === "running",
      ),
      instances,
    }),
    "db.create": (params) => {
      checkName(params.name);
      if (
        params.charset !== undefined &&
        params.charset !== null &&
        !CHARSET_RE.test(params.charset)
      ) {
        fakeError("invalid_input", "bad charset", "database_charset", { field: "charset" });
      }
      const target = running(params.instance);
      if (databases.some((db) => db.instance === target.instance && db.name === params.name)) {
        fakeError("conflict", `database ${params.name} exists`, "database_exists", {
          name: params.name,
        });
      }
      const created = qaDatabase(target, params.name, 0, 0);
      databases.push({ ...created, charset: params.charset ?? created.charset });
      return created;
    },
    "db.drop": (params) => {
      if (params.confirm !== true) {
        fakeError("invalid_input", "db.drop needs confirm: true", "confirm_required");
      }
      checkName(params.name);
      const target = running(params.instance);
      const index = databases.findIndex(
        (db) => db.instance === target.instance && db.name === params.name,
      );
      if (index < 0) fakeError("not_found", `database ${params.name} not found`);
      databases.splice(index, 1);
      return ACK;
    },
    "db.url": (params) => {
      const target = find(params.instance);
      const port = String(target.port ?? 3306);
      return {
        url: `mysql://root:s3cr3t@127.0.0.1:${port}/${params.database}`,
        env: {
          vars: {
            DB_CONNECTION: "mysql",
            DB_HOST: "127.0.0.1",
            DB_PORT: port,
            DB_DATABASE: params.database,
            DB_USERNAME: "root",
            DB_PASSWORD: "s3cr3t",
          },
        },
      };
    },
    "db.shell": (params) => {
      const target = running(params.instance);
      return {
        program: "C:/Forgebox/bin/mysql/8.4.13/bin/mysql.exe",
        args: [
          "--protocol=tcp",
          "--host=127.0.0.1",
          `--port=${String(target.port ?? 3306)}`,
          "--user=root",
          ...(params.database === undefined || params.database === null ? [] : [params.database]),
        ],
        cwd: null,
        env: { MYSQL_PWD: "s3cr3t" },
      };
    },
    "db.export": (params) => {
      const target = running(params.instance);
      checkName(params.name);
      if (!databases.some((db) => db.instance === target.instance && db.name === params.name)) {
        fakeError("not_found", `database ${params.name} not found`);
      }
      const path = params.output ?? `${BACKUPS}/${params.name}-20261104T120000Z.sql.zst`;
      if (params.name.startsWith("broken_")) {
        return fakeJob({
          steps: [{ bytes_done: 2 * MIB, speed_bps: 4 * MIB }],
          outcome: {
            failed: {
              code: "internal",
              message: `mysqldump: the dump client failed (exit 2):\nmysqldump: Got error: 1045: Access denied for user 'root'@'localhost'\nmysqldump: Couldn't execute 'SHOW TABLES'`,
            },
          },
        });
      }
      if (params.name.startsWith("hold_")) {
        return fakeJob({ steps: [{ bytes_done: 3 * MIB, speed_bps: 6 * MIB }], hold: true });
      }
      const steps = Array.from({ length: params.name.startsWith("big_") ? 8 : 3 }, (_, i) => ({
        bytes_done: (i + 1) * 5 * MIB,
        speed_bps: 12 * MIB,
        message: "dumping",
      }));
      return fakeJob({ steps, stepMs: STEP_MS, outcome: { done: { path, size_bytes: 15 * MIB } } });
    },
    "db.import": (params) => {
      const target = running(params.instance);
      checkName(params.name);
      if (!databases.some((db) => db.instance === target.instance && db.name === params.name)) {
        fakeError("not_found", `database ${params.name} not found`);
      }
      if (params.name.startsWith("broken_")) {
        return fakeJob({
          steps: [{ pct: 20 }],
          outcome: {
            failed: {
              code: "internal",
              message: `mysql: the restore client failed (exit 1):\nERROR 1064 (42000) at line 12: You have an error in your SQL syntax`,
            },
          },
        });
      }
      if (params.name.startsWith("hold_")) return fakeJob({ steps: [{ pct: 35 }], hold: true });
      return fakeJob({
        steps: [{ pct: 25 }, { pct: 60 }, { pct: 90 }],
        stepMs: STEP_MS,
        outcome: { done: ACK },
      });
    },
    "services.start": (params, { fake }) => {
      for (const ref of params.targets ?? []) {
        const found = instances.find((i) => i.instance === ref);
        if (found === undefined) fakeError("not_found", `${ref} not found`);
        found.state = "running";
        found.port = found.protocol === "postgresql" ? 5432 : 3306;
        fake.publish("service.running", {
          v: 1,
          instance_id: found.instance_id,
          name: found.instance,
          tool: found.tool,
          state: "running",
          pid: 4242,
          strategy: null,
          exit_code: null,
          error: null,
        } satisfies T.ServicePayload);
      }
      return fakeJob({ outcome: { done: { instances: [], pools: [] } } });
    },
  };
  return { instances, databases, handlers };
}

/** The scenario's handlers (a fresh state per call). */
export function databasesHandlers(): FakeHandlers {
  return createDatabasesQa().handlers;
}

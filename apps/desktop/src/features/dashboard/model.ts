// Pure helpers of the Dashboard: which tiles, which recent sites, how metric rings become
// sparkline series.
import type { InstanceDto, PoolDto, SiteDto } from "../../ipc/generated";
import type { InstanceSample, PoolSample } from "../../state";
import { poolName, targetOf } from "../services/model";

/** Recent sites shown (docs/07 section 5.2). */
export const RECENT_SITES = 6;
/** Events shown in Atividade. */
export const ACTIVITY_LIMIT = 50;
/** Height of a service tile, px: the same for the skeleton and for data. */
export const TILE_HEIGHT = 128;
export const TILE_WIDTH = 248;

export type Tile =
  | { kind: "instance"; key: string; label: string; instance: InstanceDto }
  | { kind: "pool"; key: string; label: string; pool: PoolDto };

/** One tile per instance and per pool: instances by `tool/name`, then the pools by series. */
export function tilesOf(instances: readonly InstanceDto[], pools: readonly PoolDto[]): Tile[] {
  const byInstance: Tile[] = instances
    .map((instance) => ({
      kind: "instance" as const,
      key: instance.id,
      label: targetOf(instance),
      instance,
    }))
    .sort((a, b) => a.label.localeCompare(b.label));
  const byPool: Tile[] = pools
    .map((pool) => ({
      kind: "pool" as const,
      key: pool.version_id,
      label: poolName(pool),
      pool,
    }))
    .sort((a, b) => a.label.localeCompare(b.label));
  return [...byInstance, ...byPool];
}

/** The `limit` most recently updated sites, newest first (ties by host). */
export function recentSites(sites: readonly SiteDto[], limit = RECENT_SITES): SiteDto[] {
  return [...sites]
    .sort((a, b) => {
      const delta = Date.parse(b.updated_at) - Date.parse(a.updated_at);
      return Number.isNaN(delta) || delta === 0 ? a.host.localeCompare(b.host) : delta;
    })
    .slice(0, limit);
}

export interface Series {
  cpu: number[];
  ram: number[];
}

export function instanceSeries(samples: readonly InstanceSample[]): Series {
  return {
    cpu: samples.map((sample) => sample.cpu ?? 0),
    ram: samples.map((sample) => sample.rss ?? 0),
  };
}

/** Pools report no CPU: the first series is req/s. */
export function poolSeries(samples: readonly PoolSample[]): Series {
  return {
    cpu: samples.map((sample) => sample.rps ?? 0),
    ram: samples.map((sample) => sample.rss ?? 0),
  };
}

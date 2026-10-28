// Acceptance (M2-E1-T9 #1): 1,000 events arriving in one frame cause at most one commit per
// affected entity, and a component bound to another entity renders 0 times.
import { act, render, screen } from "@testing-library/react";
import { Profiler, type ProfilerOnRenderCallback } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { createFakeBridge } from "../ipc/fakeBridge";
import type * as T from "../ipc/generated";
import { manualFrameClock } from "./frame";
import { StateLayer } from "./layer";
import { manualPageActivity } from "./page";
import { ForgeStateProvider, useInstance, useSite } from "./react";
import { IDS, populatedData, stateScenario } from "./testFixtures";

const renders = new Map<string, number>();
const commits = new Map<string, number>();

function InstanceRow({ id }: { id: string }) {
  const instance = useInstance(id);
  renders.set(id, (renders.get(id) ?? 0) + 1);
  return <p data-testid={id}>{instance?.state ?? "?"}</p>;
}

function SiteRow({ id }: { id: string }) {
  const site = useSite(id);
  renders.set(id, (renders.get(id) ?? 0) + 1);
  return <p data-testid={id}>{site?.host ?? "?"}</p>;
}

const onRender: ProfilerOnRenderCallback = (id) => {
  commits.set(id, (commits.get(id) ?? 0) + 1);
};

let layer: StateLayer | null = null;
afterEach(async () => {
  await layer?.dispose();
  layer = null;
  renders.clear();
  commits.clear();
});

function servicePayload(state: T.ServiceState): T.ServicePayload {
  return {
    v: 1,
    instance_id: IDS.mysql,
    name: "mysql/default",
    tool: "mysql",
    state,
    pid: state === "running" ? 4412 : null,
    strategy: null,
    exit_code: null,
    error: null,
  };
}

describe("event batching per frame", () => {
  it("1,000 events in one frame: ≤ 1 commit for the affected entity, 0 renders elsewhere", async () => {
    const data = populatedData();
    data.instances = data.instances.map((i) => ({ ...i, state: "stopped", pid: null }));
    const fake = createFakeBridge(stateScenario(data));
    const frame = manualFrameClock();
    layer = new StateLayer(fake, {
      frame,
      page: manualPageActivity(),
      rttIntervalMs: 0,
      snapshotDebounceMs: 0,
    });
    const runtime = layer.runtime("local");
    render(
      <ForgeStateProvider layer={layer}>
        <Profiler id="mysql" onRender={onRender}>
          <InstanceRow id={IDS.mysql} />
        </Profiler>
        <Profiler id="redis" onRender={onRender}>
          <InstanceRow id={IDS.redis} />
        </Profiler>
        <Profiler id="site" onRender={onRender}>
          <SiteRow id={IDS.site} />
        </Profiler>
      </ForgeStateProvider>,
    );
    await act(async () => {
      await runtime.start();
      await runtime.settled();
    });
    expect(screen.getByTestId(IDS.mysql).textContent).toBe("stopped");
    expect(screen.getByTestId(IDS.site).textContent).toBe("loja.test");
    const before = new Map(renders);
    const commitsBefore = new Map(commits);

    // 1,000 events for mysql reach the webview (4 bridge frames of ≤ 256 entries).
    act(() => {
      for (let i = 0; i < 1000; i += 1) {
        fake.publish(
          i % 2 === 0 ? "service.starting" : "service.running",
          servicePayload(i % 2 === 0 ? "starting" : "running"),
        );
      }
      fake.flush();
    });
    // Nothing is applied before the frame.
    expect(screen.getByTestId(IDS.mysql).textContent).toBe("stopped");
    expect(renders.get(IDS.mysql)).toBe(before.get(IDS.mysql));
    expect(frame.pending()).toBe(1);

    act(() => {
      frame.tick();
    });
    expect(screen.getByTestId(IDS.mysql).textContent).toBe("running");
    const delta = (map: Map<string, number>, prev: Map<string, number>, id: string) =>
      (map.get(id) ?? 0) - (prev.get(id) ?? 0);
    expect(delta(renders, before, IDS.mysql)).toBe(1);
    expect(delta(commits, commitsBefore, "mysql")).toBe(1);
    expect(delta(renders, before, IDS.redis)).toBe(0);
    expect(delta(commits, commitsBefore, "redis")).toBe(0);
    expect(delta(renders, before, IDS.site)).toBe(0);
    expect(delta(commits, commitsBefore, "site")).toBe(0);
    expect(frame.pending()).toBe(0);
  });
});

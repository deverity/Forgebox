import { describe, expect, it, vi } from "vitest";
import { SlotRegistry } from "./slots";

describe("SlotRegistry", () => {
  it("orders by order then id, whatever the registration order", () => {
    const registry = new SlotRegistry<{ id: string; order?: number }>("t");
    registry.register({ id: "c" });
    registry.register({ id: "b", order: 10 });
    registry.register({ id: "a" });
    expect(registry.list().map((e) => e.id)).toEqual(["b", "a", "c"]);
  });

  it("replaces an entry with the same id and unregisters only what it registered", () => {
    const registry = new SlotRegistry<{ id: string; v: number }>("t");
    const undoFirst = registry.register({ id: "x", v: 1 });
    registry.register({ id: "x", v: 2 });
    undoFirst(); // the first entry is already gone: the replacement stays
    expect(registry.list()).toEqual([{ id: "x", v: 2 }]);
  });

  it("notifies subscribers and keeps the snapshot stable between changes", () => {
    const registry = new SlotRegistry<{ id: string }>("t");
    const listener = vi.fn();
    const off = registry.subscribe(listener);
    const before = registry.list();
    expect(registry.list()).toBe(before);
    const undo = registry.register({ id: "a" });
    expect(listener).toHaveBeenCalledTimes(1);
    expect(registry.list()).not.toBe(before);
    undo();
    expect(listener).toHaveBeenCalledTimes(2);
    off();
    registry.register({ id: "b" });
    expect(listener).toHaveBeenCalledTimes(2);
  });
});

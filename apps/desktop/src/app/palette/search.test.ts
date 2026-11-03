import { describe, expect, it } from "vitest";
import { fold, score } from "./search";

describe("search", () => {
  it("ignores case and diacritics", () => {
    expect(fold("Serviços Ação")).toBe("servicos acao");
    expect(score("SERVICOS", fold("Ir para Serviços"))).toBeGreaterThan(0);
    expect(score("serviços", fold("Ir para Serviços"))).toBeGreaterThan(0);
    expect(score("servi", fold("Go to Services"))).toBeGreaterThan(0);
  });

  it("needs every word, prefers word starts and tolerates a missing letter", () => {
    const text = fold("Start mysql/default");
    expect(score("start my", text)).toBeGreaterThan(0);
    expect(score("start redis", text)).toBe(0);
    expect(score("default", text)).toBeGreaterThan(score("ault", text));
    expect(score("mysql dflt", text)).toBeGreaterThan(0);
    expect(score("", text)).toBeGreaterThan(0);
  });
});

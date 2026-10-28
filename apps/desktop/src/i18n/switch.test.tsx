import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { initI18n, setLanguage } from "./index";

describe("runtime language switch", () => {
  it("re-renders without reload and calls set_language", async () => {
    const calls: string[] = [];
    await initI18n({
      override: "en-US",
      setLanguage: (l) => {
        calls.push(l);
        return Promise.resolve();
      },
    });
    render(<App />);
    expect(screen.getByText("Your local development environment")).toBeInTheDocument();

    await act(async () => {
      await setLanguage("pt-BR");
    });
    expect(screen.getByText("Seu ambiente de desenvolvimento local")).toBeInTheDocument();
    expect(calls).toEqual(["en-US", "pt-BR"]);
  });

  it("switches through the select element", async () => {
    const bridge = vi.fn(() => Promise.resolve());
    await initI18n({ override: "en-US", setLanguage: bridge });
    render(<App />);
    await userEvent.selectOptions(screen.getByRole("combobox"), "pt-BR");
    expect(await screen.findByText("Idioma")).toBeInTheDocument();
    expect(bridge).toHaveBeenLastCalledWith("pt-BR");
  });
});

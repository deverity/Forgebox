import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { useTranslation } from "react-i18next";
import { LanguageSwitcher } from "./LanguageSwitcher";
import { initI18n, setLanguage } from "./index";

/** Picks `name` in the design-system Select (Radix: open the trigger, click the option). */
async function pick(name: string) {
  await userEvent.click(screen.getByRole("combobox"));
  await userEvent.click(await screen.findByRole("option", { name }));
}

/** What the old placeholder shell showed: a translated line and the picker. */
function App() {
  const { t } = useTranslation("shell");
  return (
    <main>
      <p>{t("tagline")}</p>
      <LanguageSwitcher />
    </main>
  );
}

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
    await pick("Português (Brasil)");
    expect(await screen.findByRole("combobox", { name: "Idioma" })).toBeInTheDocument();
    expect(bridge).toHaveBeenLastCalledWith("pt-BR");
  });

  it("initI18n resolves and App renders when the bridge rejects", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    await expect(
      initI18n({
        override: "pt-BR",
        setLanguage: () => Promise.reject(new Error("ipc unavailable")),
      }),
    ).resolves.toBeDefined();
    render(<App />);
    expect(screen.getByText("Seu ambiente de desenvolvimento local")).toBeInTheDocument();
    await vi.waitFor(() => {
      expect(warn).toHaveBeenCalled();
    });
    warn.mockRestore();
  });

  it("setLanguage still switches the UI when the bridge rejects", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    await initI18n({
      override: "en-US",
      setLanguage: () => Promise.reject(new Error("ipc unavailable")),
    });
    render(<App />);
    await act(async () => {
      await setLanguage("pt-BR");
    });
    expect(screen.getByText("Seu ambiente de desenvolvimento local")).toBeInTheDocument();
    warn.mockRestore();
  });
});

describe("language persistence (E2-02)", () => {
  it("saves the picked language in the GUI prefs", async () => {
    globalThis.localStorage.removeItem("forgebox.prefs");
    await initI18n({ override: "en-US", setLanguage: () => Promise.resolve() });
    render(<App />);
    await pick("Português (Brasil)");
    await vi.waitFor(() => {
      expect(JSON.parse(globalThis.localStorage.getItem("forgebox.prefs") ?? "{}")).toMatchObject({
        language: "pt-BR",
      });
    });
  });
});

describe("pt-BR zero plural (R-06)", () => {
  it("says 'Nenhum site' for zero, not '0 site'", async () => {
    await initI18n({ override: "pt-BR", setLanguage: () => Promise.resolve() });
    const { default: i18next } = await import("i18next");
    await i18next.loadNamespaces("sites");
    expect(i18next.t("sites:count", { count: 0 })).toBe("Nenhum site");
    expect(i18next.t("sites:count", { count: 1 })).toBe("1 site");
    expect(i18next.t("sites:count", { count: 3 })).toBe("3 sites");
  });
});

// E2-02: the language chosen in the switcher is saved in the GUI prefs and read back at boot.
import { describe, expect, it, vi } from "vitest";
import {
  GUI_PREF_DEFAULTS,
  readSavedLanguage,
  saveLanguage,
  type GuiPrefsApi,
  type GuiPrefsValues,
} from "./guiPrefs";

function api(
  values: Partial<GuiPrefsValues> | Error | "never",
  set: (patch: Partial<GuiPrefsValues>) => Promise<void> = () => Promise.resolve(),
): GuiPrefsApi {
  return {
    get: () =>
      values === "never"
        ? new Promise<GuiPrefsValues>(() => undefined)
        : values instanceof Error
          ? Promise.reject(values)
          : Promise.resolve({ ...GUI_PREF_DEFAULTS, ...values }),
    set,
  };
}

describe("saved language", () => {
  it("reads the saved language", async () => {
    expect(await readSavedLanguage(api({ language: "en-US" }))).toBe("en-US");
  });

  it("is null when nothing is saved", async () => {
    expect(await readSavedLanguage(api({}))).toBeNull();
  });

  it("is null when the read fails", async () => {
    expect(await readSavedLanguage(api(new Error("no prefs")))).toBeNull();
  });

  it("never holds the first paint: gives up after the timeout", async () => {
    expect(await readSavedLanguage(api("never"), 20)).toBeNull();
  });

  it("saves the choice as the `language` pref", async () => {
    const set = vi.fn<GuiPrefsApi["set"]>(() => Promise.resolve());
    await saveLanguage("en-US", api({}, set));
    expect(set).toHaveBeenCalledWith({ language: "en-US" });
  });

  it("only logs when saving fails", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const set = vi.fn<GuiPrefsApi["set"]>(() => Promise.reject(new Error("disk full")));
    await expect(saveLanguage("pt-BR", api({}, set))).resolves.toBeUndefined();
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });
});

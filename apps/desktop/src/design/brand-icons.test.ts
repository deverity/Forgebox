// Brand icons (docs/design/brand/README.md, M2-E1-T22): the committed files are what
// scripts/gen-icons.mjs produces from docs/design/brand, and their structure is what the EXEs
// (icon.ico) and the tray (icons/tray) expect.
import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const root = resolve(import.meta.dirname, "../..");
const repo = resolve(root, "../..");
const script = join(root, "scripts/gen-icons.mjs");
const icons = join(root, "src-tauri/icons");
const tray = join(icons, "tray");

const PNG_MAGIC = [0x89, 0x50, 0x4e, 0x47];

interface IcoEntry {
  size: number;
  png: boolean;
  data: Buffer;
}

function readIco(file: string): IcoEntry[] {
  const buf = readFileSync(file);
  expect(buf.readUInt16LE(0)).toBe(0);
  expect(buf.readUInt16LE(2)).toBe(1);
  return Array.from({ length: buf.readUInt16LE(4) }, (_, i) => {
    const e = 6 + 16 * i;
    const len = buf.readUInt32LE(e + 8);
    const off = buf.readUInt32LE(e + 12);
    const data = buf.subarray(off, off + len);
    return {
      size: buf.readUInt8(e) || 256,
      png: PNG_MAGIC.every((b, k) => data[k] === b),
      data,
    };
  });
}

const STATES = [
  "base",
  "running",
  ...Array.from({ length: 8 }, (_, i) => `starting-${String(i)}`),
  "stopped",
  "updating",
  "warning",
  "error",
];

describe("brand icons", () => {
  it("icon.ico holds 16-256 px: 32-bit BMP below 256, PNG at 256", () => {
    const entries = readIco(join(icons, "icon.ico"));
    expect(entries.map((e) => e.size)).toEqual([16, 20, 24, 32, 40, 48, 64, 256]);
    for (const e of entries) {
      expect(e.png).toBe(e.size === 256);
      if (!e.png) {
        expect(e.data.readUInt32LE(0)).toBe(40); // BITMAPINFOHEADER
        expect(e.data.readUInt16LE(14)).toBe(32); // bits per pixel
      }
    }
  });

  it("every tray state has light and dark icons at 16/20/24/32 px, all different", () => {
    const expected = [...STATES, "remote"].flatMap((s) => [`${s}-dark.ico`, `${s}-light.ico`]);
    expect(readdirSync(tray).sort()).toEqual(expected.sort());
    const seen = new Map<string, string>();
    for (const name of expected) {
      const entries = readIco(join(tray, name));
      expect(entries.map((e) => e.size)).toEqual([16, 20, 24, 32]);
      // The 16 px image identifies the icon: no two states (or themes) may look the same.
      const key = entries[0]?.data.toString("base64") ?? "";
      expect(seen.get(key), `${name} duplicates ${seen.get(key) ?? ""}`).toBeUndefined();
      seen.set(key, name);
    }
  });

  it("badges sit in the bottom-right quadrant over an untouched top-left", () => {
    // 16 px BMP rows are bottom-up BGRA after the 40-byte header.
    const pixel = (data: Buffer, x: number, y: number) =>
      data.readUInt32LE(40 + ((15 - y) * 16 + x) * 4);
    const base = readIco(join(tray, "base-dark.ico"))[0]?.data;
    expect(base).toBeDefined();
    if (!base) return;
    for (const state of STATES.slice(1)) {
      const icon = readIco(join(tray, `${state}-dark.ico`))[0]?.data;
      if (!icon) throw new Error(state);
      for (let y = 0; y < 6; y++) {
        for (let x = 0; x < 6; x++) expect(pixel(icon, x, y)).toBe(pixel(base, x, y));
      }
    }
  });

  describe("icons:check", { timeout: 180_000 }, () => {
    function run(env: Record<string, string> = {}) {
      return spawnSync(process.execPath, [script, "--check"], {
        encoding: "utf8",
        env: { ...process.env, ...env },
      });
    }

    it("exits 0 on a clean tree", () => {
      const res = run();
      expect(res.stderr).toBe("");
      expect(res.status).toBe(0);
    });

    it("exits 1 and names the file after a manual edit of one .ico", () => {
      const dir = mkdtempSync(join(tmpdir(), "forge-icons-"));
      try {
        const rel = "apps/desktop/src-tauri/icons";
        cpSync(join(repo, rel), join(dir, rel), { recursive: true });
        for (const f of ["forgebox-icon-1024.png", "contact-sheet.png"]) {
          cpSync(join(repo, "docs/design/brand", f), join(dir, "docs/design/brand", f));
        }
        const edited = join(dir, rel, "tray/running-dark.ico");
        const bytes = readFileSync(edited);
        const at = bytes.length - 100;
        bytes.writeUInt8(bytes.readUInt8(at) ^ 0xff, at);
        writeFileSync(edited, bytes);
        const res = run({ ICONS_TARGET_ROOT: dir });
        expect(res.status).toBe(1);
        expect(res.stderr).toContain("icons drift: 1 file(s)");
        expect(res.stderr).toContain("apps/desktop/src-tauri/icons/tray/running-dark.ico");
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    });
  });
});

// The README screenshot set (M2-E4-T12). Pure data shared by the capture spec
// (qa/readme-shots.spec.ts) and the vitest check (src/qa/readme.test.ts), so the images the
// README links, the files on disk and the capture command cannot drift apart.

export const SHOTS_DIR = "docs/assets/screenshots";
export const MAX_BYTES = 400 * 1024;
export const MAX_WIDTH = 1600;
/** 1280x800 CSS px at DPR 1.25 = 1600x1000 device px (crisp, at the width limit). */
export const VIEWPORT = { width: 1280, height: 800 } as const;
export const DPR = 1.25;

/** The first site of the QA scenarios (PHP vhost); `fakeId("01QAS1TE", 1)`. */
export const PHP_SITE = "01QAS1TE000000000000000001";

export type ShotAction = "none" | "palette" | "site-performance";

export interface ReadmeShot {
  /** File name without extension: `<screen>-<theme>`. */
  file: string;
  theme: "dark" | "light";
  path: string;
  action: ShotAction;
  /** Short captions (pt-BR / en-US); the README repeats them, the vitest check compares. */
  pt: string;
  en: string;
}

export const SHOTS: readonly ReadmeShot[] = [
  {
    file: "dashboard-dark",
    theme: "dark",
    path: "/dashboard",
    action: "none",
    pt: "Painel: serviços, sites e atividade num relance.",
    en: "Dashboard (Painel): services, sites and activity at a glance.",
  },
  {
    file: "services-dark",
    theme: "dark",
    path: "/services",
    action: "none",
    pt: "Serviços: várias versões de cada ferramenta, iniciar e parar com um clique.",
    en: "Services: several versions of each tool, start and stop in one click.",
  },
  {
    file: "sites-dark",
    theme: "dark",
    path: "/sites",
    action: "none",
    pt: "Sites: domínios locais com HTTPS e a versão de PHP de cada um.",
    en: "Sites: local domains with HTTPS and the PHP version of each one.",
  },
  {
    file: "site-detail-dark",
    theme: "dark",
    path: `/sites/${PHP_SITE}`,
    action: "none",
    pt: "Detalhe do site: PHP por vhost, servidor web, certificado e processos.",
    en: "Site detail: per-vhost PHP, web server, certificate and processes.",
  },
  {
    file: "performance-dark",
    theme: "dark",
    path: `/sites/${PHP_SITE}`,
    action: "site-performance",
    pt: "Desempenho do site: latência, vazão, rotas e erros a partir do access log.",
    en: "Site performance: latency, throughput, routes and errors from the access log.",
  },
  {
    file: "catalog-dark",
    theme: "dark",
    path: "/catalog",
    action: "none",
    pt: "Catálogo: ferramentas em YAML instaladas sob demanda, com hash verificado.",
    en: "Catalog: YAML-described tools installed on demand, hash-verified.",
  },
  {
    file: "palette-dark",
    theme: "dark",
    path: "/dashboard",
    action: "palette",
    pt: "Paleta de comandos (Ctrl+K): tudo o que a interface faz, pelo teclado.",
    en: "Command palette (Ctrl+K): everything the interface does, from the keyboard.",
  },
  {
    file: "dashboard-light",
    theme: "light",
    path: "/dashboard",
    action: "none",
    pt: "O mesmo Painel no tema claro.",
    en: "The same Dashboard in the light theme.",
  },
];

/** Repo-relative path of an image. */
export function imagePath(shot: ReadmeShot): string {
  return `${SHOTS_DIR}/${shot.file}.png`;
}

/** Strings that must never show up in the README (personal data). */
export const FORBIDDEN_IN_TEXT: readonly RegExp[] = [/gustavo/i, /C:\\Users/i];

import type { IconName } from "../features";

// 16 px stroke icons (docs/07 section 2.4). Decorative: the label is always next to them, or
// on the control that holds them.
const PATHS: Record<IconName, string> = {
  dashboard: "M2 2.5h5v5H2zM9 2.5h5v3H9zM9 7.5h5v6H9zM2 9.5h5v4H2z",
  services: "M2.5 3h11v3.5h-11zM2.5 9.5h11V13h-11zM5 4.75h.01M5 11.25h.01",
  sites:
    "M8 2a6 6 0 1 0 0 12A6 6 0 0 0 8 2zM2 8h12M8 2c2 2 2.5 4 2.5 6S10 12 8 14c-2-2-2.5-4-2.5-6S6 4 8 2z",
  php: "M2 4.5l6-2.5 6 2.5v7L8 14l-6-2.5zM2 4.5L8 7l6-2.5M8 7v7",
  databases:
    "M3 4.5C3 3.4 5.2 2.5 8 2.5s5 .9 5 2-2.2 2-5 2-5-.9-5-2zM3 4.5v7c0 1.1 2.2 2 5 2s5-.9 5-2v-7M3 8c0 1.1 2.2 2 5 2s5-.9 5-2",
  mail: "M2 3.5h12v9H2zM2 4l6 5 6-5",
  logs: "M3 2.5h10v11H3zM5.5 5.5h5M5.5 8h5M5.5 10.5h3",
  catalog: "M2.5 2.5h4.5v4.5H2.5zM9 2.5h4.5v4.5H9zM2.5 9H7v4.5H2.5zM9 9h4.5v4.5H9z",
  terminal: "M2 3h12v10H2zM4.5 6l2 2-2 2M8 10.5h3.5",
  settings:
    "M8 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5zM8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4",
};

export function Icon({ name }: { name: IconName }) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

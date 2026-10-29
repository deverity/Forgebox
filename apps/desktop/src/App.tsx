import { RouterProvider } from "@tanstack/react-router";
import { useState } from "react";
import { createAppRouter, type AppRouter } from "./app/router";
import { ForgeStateProvider, type StateLayer } from "./state";

export { GALLERY_PATH } from "./app/routes/root";

export interface AppProps {
  /** The data layer of the app (one bridge, one runtime per context). */
  layer: StateLayer;
  /** Defaults to the browser-history router; tests pass a memory-history one. */
  router?: AppRouter;
}

/**
 * The app: the state layer around the router. The shell lives in the route tree
 * (`app/router.tsx`, M2-E1-T10): the root route calls `useFirstFrameReady()` (the window is shown
 * on `ready`), and the daemon connection starts in an effect after the first commit, so the
 * shell never waits for the daemon to paint.
 */
export function App({ layer, router }: AppProps) {
  const [fallbackRouter] = useState(() => (router === undefined ? createAppRouter() : null));
  const active = router ?? fallbackRouter;
  if (active === null) return null;
  return (
    <ForgeStateProvider layer={layer}>
      <RouterProvider router={active} />
    </ForgeStateProvider>
  );
}

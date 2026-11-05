import { createRoute, lazyRouteComponent } from "@tanstack/react-router";
import i18n from "i18next";
import { GALLERY_PATH, rootRoute } from "./root";

/**
 * The component gallery (M2-E1-T11): dev, test and qa builds only. Vite replaces the MODE
 * check statically, so the production build drops the route and its chunk.
 */
export const galleryRoute =
  import.meta.env.MODE === "production"
    ? null
    : createRoute({
        getParentRoute: () => rootRoute,
        path: GALLERY_PATH,
        component: lazyRouteComponent(async () => {
          await i18n.loadNamespaces("gallery");
          return import("../gallery/Gallery");
        }, "default"),
      });

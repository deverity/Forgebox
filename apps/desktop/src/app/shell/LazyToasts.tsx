import { Suspense, lazy } from "react";

// The toast viewport pulls in motion and Radix; it mounts after the first paint as its own chunk.
// Toasts raised before it loads wait in the module-level store and show as soon as it is there.
const ToastViewport = lazy(async () => {
  const module = await import("../../design/primitives/Toast");
  return { default: module.ToastViewport };
});

export function LazyToasts() {
  return (
    <Suspense fallback={null}>
      <ToastViewport />
    </Suspense>
  );
}

// Atenção sources owned by the Dashboard (M2-E2-T14, docs/07 section 5.2). M2-E5 registers the
// performance alerts (docs/14 section 5) and the "projects" section the same way.
import { registerAttentionSource } from "../../app/slots";
import {
  useCaItems,
  useCertItems,
  useDnsItems,
  useDriftItems,
  useFailedItems,
  usePortItems,
  useUpdateItems,
} from "./sources";

registerAttentionSource({ id: "failed-services", order: 10, useItems: useFailedItems });
registerAttentionSource({ id: "port-conflicts", order: 20, useItems: usePortItems });
registerAttentionSource({ id: "ca-status", order: 30, useItems: useCaItems });
registerAttentionSource({ id: "certificates", order: 40, useItems: useCertItems });
registerAttentionSource({ id: "dns", order: 50, useItems: useDnsItems });
registerAttentionSource({ id: "config-drift", order: 60, useItems: useDriftItems });
registerAttentionSource({ id: "tool-updates", order: 70, useItems: useUpdateItems });

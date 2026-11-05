import { LogViewer } from "../../logs/LogViewer";

export interface LogsTabProps {
  /** `mysql/default`: the target `logs.subscribe` takes. */
  target: string;
}

/** The "Logs" tab of the service sheet: the shared LogViewer on one target (M2-E3-T12). */
export function LogsTab({ target }: LogsTabProps) {
  return <LogViewer targets={[target]} name={target} height={320} />;
}

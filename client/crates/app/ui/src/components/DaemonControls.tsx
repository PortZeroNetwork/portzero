import { useState } from "react";
import { startDaemon, stopDaemon, restartDaemon } from "../api";
import type { Status } from "../types";

interface Props {
  status: Status;
  onError: (message: string) => void;
  onChanged: () => void;
}

/** Start / Stop / Restart, contextual to whether the daemon is running. */
export default function DaemonControls({ status, onError, onChanged }: Props) {
  const [busy, setBusy] = useState(false);

  async function act(fn: () => Promise<void>) {
    setBusy(true);
    try {
      await fn();
      // Give the CLI a beat to write/remove its PID file before we re-read.
      setTimeout(onChanged, 800);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="button-row">
      {status.running ? (
        <>
          <button
            className="button"
            disabled={busy}
            onClick={() => act(restartDaemon)}
          >
            Restart daemon
          </button>
          <button
            className="button ghost"
            disabled={busy}
            onClick={() => act(stopDaemon)}
          >
            Stop daemon
          </button>
        </>
      ) : (
        <button
          className="button"
          disabled={busy}
          onClick={() => act(startDaemon)}
        >
          Start daemon
        </button>
      )}
      {busy && <span className="spin" aria-label="working" />}
    </div>
  );
}

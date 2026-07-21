import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getStatus } from "./api";
import type { Status } from "./types";
import DaemonControls from "./components/DaemonControls";
import Settings from "./components/Settings";
import Tunnels from "./components/Tunnels";
import Issues from "./components/Issues";
import NextSteps from "./components/NextSteps";
import mark from "./assets/portzero-mark.jpg";

const POLL_MS = 4000;

interface Health {
  glyph: "ok" | "warn" | "bad";
  summary: string;
  sub: string;
}

function health(status: Status | null): Health {
  if (!status) {
    return { glyph: "warn", summary: "Connecting to daemon…", sub: "" };
  }
  if (!status.running) {
    return {
      glyph: "bad",
      summary: "Daemon is not running",
      sub:
        status.status_message ??
        "Start the daemon to discover tunnels and run examples.",
    };
  }
  const problems = status.problems ?? [];
  if (problems.length > 0) {
    return {
      glyph: "warn",
      summary: `${problems.length} issue${problems.length === 1 ? "" : "s"} need attention`,
      sub: "See Issues below for fixes.",
    };
  }
  const localCount = status.local_services?.length ?? 0;
  const cloudCount = status.cloud_routes?.length ?? 0;
  return {
    glyph: "ok",
    summary: "Everything looks healthy",
    sub: `${localCount} local · ${cloudCount} cloud tunnel${
      localCount + cloudCount === 1 ? "" : "s"
    }`,
  };
}

export default function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await getStatus());
    } catch (e) {
      // get_status never errors by contract, but guard anyway.
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    window.scrollTo(0, 0);
    refresh();
    timer.current = window.setInterval(refresh, POLL_MS);
    const unlistenP = listen("app://focus", () => {
      window.scrollTo(0, 0);
      refresh();
    });
    return () => {
      if (timer.current) window.clearInterval(timer.current);
      unlistenP.then((u) => u());
    };
  }, [refresh]);

  const h = health(status);
  const pid = status?.daemon_pid;

  return (
    <>
      <header className="topbar">
        <nav className="shell nav" aria-label="Main">
          <span className="brand">
            <img src={mark} alt="" aria-hidden="true" />
            <span>PortZero</span>
          </span>
          <span className="nav-status">
            {status
              ? status.running
                ? pid
                  ? `daemon running · pid ${pid}`
                  : "daemon running"
                : "daemon stopped"
              : ""}
          </span>
        </nav>
      </header>

      <main className="shell">
        <h1>Local dashboard</h1>
        <div className="health">
          <span className={`glyph ${h.glyph}`} />
          <span>
            <span className="summary">{h.summary}</span>
            {h.sub && (
              <>
                <br />
                <span className="sub">{h.sub}</span>
              </>
            )}
          </span>
        </div>

        {status && (
          <>
            <NextSteps status={status} onError={setError} onChanged={refresh} />
            <Tunnels status={status} onError={setError} />
            <Settings status={status} onError={setError} onChanged={refresh} />

            <section className="section">
              <div className="section-head">
                <h2>Daemon</h2>
                <p className="section-note">
                  The daemon discovers tunnels and serves the local API.
                </p>
              </div>
              <DaemonControls
                status={status}
                onError={setError}
                onChanged={refresh}
              />
            </section>

            <Issues status={status} />
          </>
        )}
      </main>

      {error && (
        <div className="toast" role="alert">
          {error}
          <div>
            <button className="button ghost" onClick={() => setError(null)}>
              Dismiss
            </button>
          </div>
        </div>
      )}
    </>
  );
}

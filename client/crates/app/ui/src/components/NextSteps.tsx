import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  downloadExamples,
  runExample,
  stopExample,
  openExternal,
} from "../api";
import type { Status, Example, ExampleLog, ExampleEnd } from "../types";

interface Props {
  status: Status;
  onError: (message: string) => void;
  onChanged: () => void;
}

interface TermLine {
  text: string;
  kind: "out" | "cmd" | "error" | "end";
}

// ---------------------------------------------------------------------------
// Small, durable "have they done this yet?" flags. Onboarding progress should
// survive an app restart, so once a step is completed we remember it in
// localStorage rather than re-deriving it purely from live daemon status (an
// example the user already ran, then stopped, still counts as "done").
// ---------------------------------------------------------------------------

function flagGet(key: string): boolean {
  try {
    return window.localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}
function flagSet(key: string): void {
  try {
    window.localStorage.setItem(key, "1");
  } catch {
    // Private-mode / storage-disabled: fall back to in-session derivation.
  }
}

const RAN_EXAMPLE_KEY = "pz.onboarding.ranExample";
const WIRED_OWN_KEY = "pz.onboarding.wiredOwnCode";
const COLLAPSED_KEY = "pz.onboarding.collapsed";

function stripAnsi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return String(s)
    .replace(/\x1b\[[0-9;?=]*[A-Za-z]/g, "")
    .replace(/\x1b[=>]/g, "")
    .replace(/\r/g, "");
}

function normalizeDomain(d: string | undefined | null): string {
  return String(d ?? "")
    .replace(/:80$/, "")
    .replace(/:443$/, "")
    .toLowerCase();
}

function osId(status: Status): string {
  return status.environment?.os?.id ?? "linux";
}
function dockerInstalled(status: Status): boolean {
  return !!status.environment?.docker?.installed;
}
function langInstalled(status: Status, langId: string): boolean {
  const items = status.languages?.items ?? [];
  return !!items.find((x) => x.id === langId)?.installed;
}
function exampleCommand(ex: Example, os: string): string {
  const c = ex.commands ?? {};
  return c[os] || c.linux || c.macos || c.windows || "";
}
function exampleUrl(ex: Example): string {
  return "http://" + normalizeDomain(ex.domain) + "/";
}
function exampleRunnable(status: Status, ex: Example): boolean {
  if (ex.requires_docker && !dockerInstalled(status)) return false;
  return langInstalled(status, ex.language);
}
function sortedExamples(status: Status): Example[] {
  const all = (status.getting_started?.examples ?? []).slice();
  const guess = status.languages?.default;
  const rank = (ex: Example) =>
    (exampleRunnable(status, ex) ? 0 : 1) * 10 +
    (ex.language === guess ? 0 : 1);
  all.sort((a, b) => {
    const r = rank(a) - rank(b);
    if (r !== 0) return r;
    return (a.title ?? a.id).localeCompare(b.title ?? b.id);
  });
  return all;
}

/** The set of `.portzero.local` domains that belong to bundled examples. */
function exampleDomainSet(status: Status): Set<string> {
  const set = new Set<string>();
  for (const ex of status.getting_started?.examples ?? []) {
    if (ex.domain) set.add(normalizeDomain(ex.domain));
  }
  return set;
}

/** Local tunnels the user wired to their OWN code (not a bundled example). */
function ownLocalTunnels(status: Status) {
  const examples = exampleDomainSet(status);
  return (status.local_services ?? []).filter(
    (s) => !examples.has(normalizeDomain(s.domain)),
  );
}

interface StepMeta {
  n: number;
  title: string;
  done: boolean;
}

export default function NextSteps({ status, onError, onChanged }: Props) {
  const examples = status.examples;
  const running = examples?.running ?? [];
  const downloadState = examples?.download_state;
  const downloadReady = !!examples?.downloaded || downloadState === "ready";
  const downloadError =
    downloadState === "error" ? examples?.download_error || "" : "";
  const downloadInProgress = !downloadReady && !downloadError;

  // Live example console state.
  const [activeId, setActiveId] = useState<string | null>(null);
  const [activeTitle, setActiveTitle] = useState<string>("");
  const [lines, setLines] = useState<TermLine[]>([]);
  const [retrying, setRetrying] = useState(false);
  const [showMore, setShowMore] = useState(false);
  const termRef = useRef<HTMLPreElement>(null);
  const activeIdRef = useRef<string | null>(null);
  activeIdRef.current = activeId;

  // Derived onboarding progress. Persisted flags OR-ed with live status so a
  // freshly-completed step lights up immediately and stays lit after restart.
  const exampleDomains = useMemo(() => exampleDomainSet(status), [status]);
  const liveExampleTunnel = (status.local_services ?? []).some((s) =>
    exampleDomains.has(normalizeDomain(s.domain)),
  );
  const own = ownLocalTunnels(status);

  const ranExample =
    flagGet(RAN_EXAMPLE_KEY) || running.length > 0 || liveExampleTunnel;
  const wiredOwn = flagGet(WIRED_OWN_KEY) || own.length > 0;

  useEffect(() => {
    if (running.length > 0 || liveExampleTunnel) flagSet(RAN_EXAMPLE_KEY);
  }, [running.length, liveExampleTunnel]);
  useEffect(() => {
    if (own.length > 0) flagSet(WIRED_OWN_KEY);
  }, [own.length]);

  const steps: StepMeta[] = [
    { n: 1, title: "Run an example", done: ranExample },
    { n: 2, title: "Wire up your own code", done: wiredOwn },
    { n: 3, title: "Get back to building", done: ranExample && wiredOwn },
  ];
  const allDone = ranExample && wiredOwn;

  // Tri-state so an explicit choice sticks across restarts:
  //   "1" = collapsed, "0" = explicitly expanded, absent = decide automatically.
  function collapsePref(): "1" | "0" | null {
    try {
      const v = window.localStorage.getItem(COLLAPSED_KEY);
      return v === "1" || v === "0" ? v : null;
    } catch {
      return null;
    }
  }
  const [collapsed, setCollapsed] = useState<boolean>(() => collapsePref() === "1");
  function collapse() {
    flagSet(COLLAPSED_KEY);
    setCollapsed(true);
  }
  function expand() {
    try {
      window.localStorage.setItem(COLLAPSED_KEY, "0");
    } catch {
      // Ignore — see flagSet.
    }
    setCollapsed(false);
  }
  // Auto-collapse the first time everything is done, but only when the user has
  // never made an explicit choice (pref still absent).
  useEffect(() => {
    if (allDone && collapsePref() === null) collapse();
  }, [allDone]);

  // Subscribe once to the streamed example output.
  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let mounted = true;
    listen<ExampleLog>("example://log", (e) => {
      if (e.payload.id !== activeIdRef.current) return;
      setLines((prev) =>
        prev.concat({
          text: stripAnsi(e.payload.line),
          kind: e.payload.line.startsWith("$") ? "cmd" : e.payload.kind,
        }),
      );
    }).then((u) => (mounted ? unlisteners.push(u) : u()));
    listen<ExampleEnd>("example://end", (e) => {
      if (e.payload.id !== activeIdRef.current) return;
      if (e.payload.message) {
        setLines((prev) =>
          prev.concat({ text: e.payload.message, kind: "end" }),
        );
      }
      setActiveId(null);
      onChanged();
    }).then((u) => (mounted ? unlisteners.push(u) : u()));
    return () => {
      mounted = false;
      unlisteners.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keep the console scrolled to the bottom as lines arrive.
  useEffect(() => {
    const el = termRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  async function run(ex: Example) {
    setActiveId(ex.id);
    setActiveTitle(ex.title ?? ex.id);
    setLines([]);
    flagSet(RAN_EXAMPLE_KEY); // running one example is enough to complete step 1
    try {
      await runExample(ex.id);
    } catch (e) {
      setActiveId(null);
      onError(String(e));
    }
    onChanged();
  }

  async function stop(id: string) {
    setLines((prev) => prev.concat({ text: "[stopping…]", kind: "end" }));
    try {
      await stopExample(id);
    } catch (e) {
      onError(String(e));
    }
  }

  async function retryDownload() {
    setRetrying(true);
    try {
      await downloadExamples();
    } catch (e) {
      onError(String(e));
    } finally {
      setRetrying(false);
      onChanged();
    }
  }

  const os = osId(status);
  const list = useMemo(() => sortedExamples(status), [status]);
  const featured = list[0];
  const rest = list.slice(1);

  // Suggest a PZ_TUNNEL name for step 2 based on the OS username if we have it
  // from a live tunnel; otherwise a friendly placeholder.
  const suggestedOwnName = "web.myapp.portzero.local";

  function stepBody(n: number) {
    if (n === 1) return step1();
    if (n === 2) return step2();
    return step3();
  }

  function renderExampleCard(ex: Example, featuredCard: boolean) {
    const runnable = exampleRunnable(status, ex);
    const isRunning = running.includes(ex.id);
    const cmd = exampleCommand(ex, os);
    return (
      <article
        key={ex.id}
        className={
          "example" +
          (runnable ? "" : " dim") +
          (featuredCard ? " featured" : "")
        }
      >
        <div className="example-head">
          <span className="tag lang">{ex.language_label ?? ex.language}</span>
          <span className="tag">{ex.variant_label ?? ex.variant}</span>
        </div>
        <h3>{ex.title ?? ex.id}</h3>
        <pre className="code-row">
          <code>{`$ cd ${ex.path}\n$ ${cmd}`}</code>
        </pre>
        <div className="example-actions">
          {!runnable ? (
            <span className="hint">
              Install {ex.language_label ?? ex.language}
              {ex.requires_docker && !dockerInstalled(status)
                ? " and Docker"
                : ""}{" "}
              to run this.
            </span>
          ) : downloadInProgress ? (
            <>
              <button className="button" disabled>
                Run
              </button>
              <span className="hint">
                <span className="spin" /> Setting up examples…
              </span>
            </>
          ) : downloadError ? (
            <span className="hint">Examples aren’t ready yet — see below.</span>
          ) : isRunning ? (
            <>
              <button className="button stop" onClick={() => stop(ex.id)}>
                Stop
              </button>
              <button
                className="button ghost"
                onClick={() =>
                  openExternal(exampleUrl(ex)).catch((e) => onError(String(e)))
                }
              >
                Open in browser
              </button>
            </>
          ) : (
            <button
              className="button"
              disabled={activeId !== null}
              onClick={() => run(ex)}
            >
              Run it
            </button>
          )}
        </div>
      </article>
    );
  }

  function step1() {
    return (
      <>
        <p className="step-lead">
          No setup — we already fetched a small example into{" "}
          <code>~/.portzero/examples</code>. Run it and PortZero gives it a real
          name like <code>{normalizeDomain(featured?.domain) || "example.portzero.local"}</code>{" "}
          instead of a <code>localhost</code> port.
        </p>

        {downloadError && (
          <div className="banner">
            {downloadError}
            <div>
              <button
                className="button ghost"
                disabled={retrying}
                onClick={retryDownload}
              >
                {retrying ? "Retrying…" : "Retry download"}
              </button>
            </div>
          </div>
        )}

        {activeId !== null && (
          <div className="console">
            <div className="console-head">
              <span className="console-title">
                <span className="spin" /> {activeTitle}
              </span>
              <button className="button stop" onClick={() => stop(activeId)}>
                Stop
              </button>
            </div>
            <pre className="term" ref={termRef} aria-live="polite">
              {lines.map((l, i) => (
                <span
                  key={i}
                  className={
                    l.kind === "cmd"
                      ? "cmd"
                      : l.kind === "error"
                        ? "err"
                        : l.kind === "end"
                          ? "end"
                          : undefined
                  }
                >
                  {l.text + "\n"}
                </span>
              ))}
            </pre>
          </div>
        )}

        {featured ? (
          <div className="examples">{renderExampleCard(featured, true)}</div>
        ) : (
          <p className="empty">No examples are available for this build.</p>
        )}

        {rest.length > 0 && (
          <div className="more-examples">
            <button
              className="disclosure"
              onClick={() => setShowMore((v) => !v)}
              aria-expanded={showMore}
            >
              {showMore ? "▾" : "▸"} More examples ({rest.length}) — run as many
              at once as you like
            </button>
            {showMore && (
              <div className="examples">
                {rest.map((ex) => renderExampleCard(ex, false))}
              </div>
            )}
          </div>
        )}
      </>
    );
  }

  function step2() {
    return (
      <>
        <p className="step-lead">
          Do the same for your own app: set <code>PZ_TUNNEL</code> in front of
          the command you already use to start it. Nothing else to install.
        </p>
        <pre className="code-row">
          <code>{`$ PZ_TUNNEL=${suggestedOwnName}:PORT <your dev command>`}</code>
        </pre>
        {wiredOwn ? (
          <p className="no-issues">
            ✓ Your tunnel is live
            {own[0] ? (
              <>
                {" "}
                — <strong>{own[0].domain}</strong>
              </>
            ) : null}
            . See <em>Tunnels</em> below for the full list.
          </p>
        ) : (
          <p className="empty">
            No tunnels of your own yet — start your app with{" "}
            <code>PZ_TUNNEL</code> set and it appears here (and under{" "}
            <em>Tunnels</em>) automatically.
          </p>
        )}
      </>
    );
  }

  function step3() {
    return (
      <>
        <p className="step-lead">
          That’s it — you’re set up. Everything you start with{" "}
          <code>PZ_TUNNEL</code> gets a real name automatically, so you can get
          back to building. 🎉
        </p>
        <p className="hint">
          Pro tip: put a placeholder in the name and PortZero fills it in per
          checkout, so branches don’t collide — e.g.{" "}
          <code>{"web.{branch}.myapp.portzero.local"}</code> becomes{" "}
          <code>web.main.myapp.portzero.local</code> on <code>main</code>.
        </p>
      </>
    );
  }

  // Collapsed summary bar once onboarding is complete.
  if (collapsed) {
    return (
      <section className="section">
        <div className="onboard-collapsed">
          <span className="no-issues">✓ You’re all set up</span>
          <button className="button ghost" onClick={expand}>
            Show getting-started steps
          </button>
        </div>
      </section>
    );
  }

  return (
    <section className="section">
      <div className="section-head">
        <h2>Getting started</h2>
        <p className="section-note">
          Three steps to using PortZero on your own code.
        </p>
      </div>

      <ol className="stepper">
        {steps.map((s) => (
          <li
            key={s.n}
            className={
              "step" + (s.done ? " done" : "") + (s.n === 3 && allDone ? " celebrate" : "")
            }
          >
            <span className="step-num" aria-hidden="true">
              {s.done ? "✓" : s.n}
            </span>
            <div className="step-main">
              <h3 className="step-title">{s.title}</h3>
              {stepBody(s.n)}
            </div>
          </li>
        ))}
      </ol>

      {allDone && (
        <div className="onboard-collapsed">
          <button className="button ghost" onClick={collapse}>
            Hide getting-started steps
          </button>
        </div>
      )}
    </section>
  );
}

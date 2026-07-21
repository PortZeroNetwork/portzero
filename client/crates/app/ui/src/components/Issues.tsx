import type { Status, Problem } from "../types";

interface Props {
  status: Status;
}

function ProblemRow({ p }: { p: Problem }) {
  const sev = (p.severity ?? "error").toLowerCase();
  const title =
    p.pid && !p.title.includes(`pid ${p.pid}`)
      ? `${p.title} (pid ${p.pid})`
      : p.title;
  return (
    <div className={`diag ${sev}`}>
      <div className="diag-title">
        [{sev.toUpperCase()}] {title}
      </div>
      {p.detail && <div className="diag-detail">{p.detail}</div>}
      {p.fix && (
        <div className="diag-fix">
          {p.fix}
          {p.fix_command && (
            <>
              {" — "}
              <code>{p.fix_command}</code>
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** Problems (duplicate names, port conflicts, failed checks) with fix hints —
 *  the same ranked list the tray and old dashboard render. */
export default function Issues({ status }: Props) {
  const problems: Problem[] = status.problems ?? [];
  const ran = status.diagnostics_checks_run;
  return (
    <section className="section">
      <div className="section-head">
        <h2>Issues</h2>
      </div>
      {problems.length === 0 ? (
        <p className="no-issues">
          no issues detected
          {ran ? ` (${ran} checks passed)` : ""}
        </p>
      ) : (
        problems.map((p, i) => <ProblemRow key={i} p={p} />)
      )}
    </section>
  );
}

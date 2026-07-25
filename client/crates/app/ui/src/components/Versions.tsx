import type { VersionReport } from "../types";

interface Props {
  report: VersionReport | null;
  onRecheck: () => void;
}

/** Which PortZero components are running, at which version, and whether they
 *  agree. A disagreement means an upgrade replaced the binaries while an older
 *  daemon, tray, or app kept running — so the panel shows what to do about it,
 *  not just that something is off. */
export default function Versions({ report, onRecheck }: Props) {
  return (
    <section className="section">
      <div className="section-head">
        <h2>Version</h2>
        <p className="section-note">
          PortZero installs several programs. They should all be from the same
          release.
        </p>
      </div>

      {!report ? (
        <p className="empty">Checking component versions…</p>
      ) : (
        <>
          <div className="status-row">
            <span className={`dot ${report.consistent ? "ok" : ""}`} />
            <span>{report.summary}</span>
          </div>

          <table>
            <thead>
              <tr>
                <th>Component</th>
                <th>Version</th>
                <th>State</th>
              </tr>
            </thead>
            <tbody>
              {report.components.map((c) => (
                <tr key={c.component}>
                  <td>{c.label}</td>
                  <td>{c.version ?? "unknown"}</td>
                  <td className="muted">{c.detail}</td>
                </tr>
              ))}
            </tbody>
          </table>

          {!report.consistent && (
            <div className="diag warning version-mismatch">
              <div className="diag-title">Components are on different versions</div>
              {report.next_steps.map((step, i) => (
                <div className="diag-fix" key={i}>
                  {step}
                </div>
              ))}
            </div>
          )}

          <div className="button-row version-actions">
            <button className="button ghost" onClick={onRecheck}>
              Check again
            </button>
          </div>
        </>
      )}
    </section>
  );
}

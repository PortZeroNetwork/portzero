import { useState } from "react";
import { setHttps } from "../api";
import type { Status } from "../types";

interface Props {
  status: Status;
  onError: (message: string) => void;
  onChanged: () => void;
}

/** The one writable setting the old dashboard exposed: enable HTTPS (443) for
 *  plain-HTTP (port 80) tunnels. Written to config.toml through the daemon. */
export default function Settings({ status, onError, onChanged }: Props) {
  const policy = status.https_policy;
  const [busy, setBusy] = useState(false);
  const enabled = !!policy?.enable_for_port_80;

  async function toggle(next: boolean) {
    setBusy(true);
    try {
      await setHttps(next);
      setTimeout(onChanged, 600);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="section">
      <div className="section-head">
        <h2>Settings</h2>
        <p className="section-note">
          A running daemon applies this within a couple of seconds.
        </p>
      </div>
      <div className="settings">
        <label>
          <input
            type="checkbox"
            checked={enabled}
            disabled={busy}
            onChange={(e) => toggle(e.target.checked)}
          />
          <span>
            <span className="setting-title">
              Enable HTTPS for HTTP tunnels
            </span>
            <p className="hint">
              When an app serves plain HTTP on port 80 at a{" "}
              <code>*.portzero.local</code> name, also expose it over HTTPS on
              443 with daemon-side TLS. If HTTPS doesn&apos;t fully work in your
              browser, Cloud tunnels always do.
            </p>
          </span>
        </label>
      </div>
    </section>
  );
}

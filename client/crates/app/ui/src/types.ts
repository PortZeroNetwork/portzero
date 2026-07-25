// Shapes mirror the daemon's /status.json (see
// client/crates/daemon/src/management/handlers/dashboard.rs::status_json).
// Fields the UI doesn't use are omitted; everything is optional because the
// daemon-down fallback object (core::fallback_status) only fills a subset.

export interface RouteAlert {
  severity?: string;
  title: string;
  detail: string;
}

export interface LocalService {
  domain: string;
  domain_template?: string;
  real_addr?: string;
  service_port?: number;
  pid?: number;
  link_url?: string | null;
  substitutions?: Record<string, string>;
  alerts?: RouteAlert[];
}

export interface CloudRoute {
  domain: string;
  domain_template?: string;
  status?: string;
  port?: number;
  pid?: number;
  link_url?: string | null;
  substitutions?: Record<string, string>;
  alerts?: RouteAlert[];
}

export interface Problem {
  severity?: string;
  title: string;
  detail?: string;
  fix?: string;
  fix_command?: string;
  pid?: number;
  needs_login?: boolean;
}

export interface HttpsPolicy {
  enable_for_port_80: boolean;
  redirect_port_80: boolean;
  passthrough_port_443: boolean;
}

export type ExamplesDownloadState =
  | "absent"
  | "downloading"
  | "ready"
  | "error";

export interface ExamplesState {
  downloaded: boolean;
  /** Coarse state of the automatic background download (no manual button). */
  download_state?: ExamplesDownloadState;
  /** User-facing reason when download_state is "error". */
  download_error?: string | null;
  dir?: string;
  path?: string;
  running: string[];
}

export interface LanguageItem {
  id: string;
  label: string;
  installed: boolean;
}

export interface Languages {
  default?: string;
  items?: LanguageItem[];
}

export interface DockerEnv {
  installed?: boolean;
}
export interface OsEnv {
  id?: string;
}
export interface Environment {
  os?: OsEnv;
  docker?: DockerEnv;
}

export interface Example {
  id: string;
  title?: string;
  language: string;
  language_label?: string;
  variant?: string;
  variant_label?: string;
  path: string;
  domain?: string;
  requires_docker?: boolean;
  commands?: Record<string, string>;
}

export interface GettingStarted {
  examples?: Example[];
}

export interface Status {
  running: boolean;
  reachable?: boolean;
  daemon_pid?: number | null;
  overlay_active?: boolean;
  auth_authenticated?: boolean;
  local_services?: LocalService[];
  cloud_connected?: boolean;
  cloud_plan?: string | null;
  cloud_error?: string | null;
  cloud_can_use_tunnels?: boolean;
  cloud_routes?: CloudRoute[];
  problems?: Problem[];
  diagnostics_checks_run?: number | null;
  https_policy?: HttpsPolicy;
  examples?: ExamplesState;
  languages?: Languages;
  environment?: Environment;
  getting_started?: GettingStarted;
  status_message?: string;
}

// Mirrors portzero_daemon::versions::{ComponentVersion, VersionReport}, which
// the app exposes through the `get_versions` command.

export type ComponentId = "app" | "tray" | "daemon" | "cli";

export type VersionSource = "running" | "installed" | "unknown";

export interface ComponentVersion {
  component: ComponentId;
  label: string;
  /** null when the component's version could not be determined. */
  version: string | null;
  running: boolean;
  pid: number | null;
  source: VersionSource;
  /** Where the version came from, or why it is missing. */
  detail: string;
}

export interface VersionReport {
  build_version: string;
  components: ComponentVersion[];
  consistent: boolean;
  summary: string;
  /** How to fix a mismatch; empty when everything agrees. */
  next_steps: string[];
}

/** One line delivered over the `example://log` event. */
export interface ExampleLog {
  id: string;
  line: string;
  kind: "out" | "cmd" | "error";
}

/** Payload of the `example://end` event. */
export interface ExampleEnd {
  id: string;
  message: string;
}

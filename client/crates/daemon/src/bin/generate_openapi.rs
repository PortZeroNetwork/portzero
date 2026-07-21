//! Regenerates `api/management-v1.yaml` from the utoipa annotations on the
//! management API handlers. Run via `just openapi`.

use utoipa::OpenApi;

fn main() -> anyhow::Result<()> {
    let yaml = portzero_daemon::management::openapi::ManagementApiDoc::openapi().to_yaml()?;

    let out_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../api/management-v1.yaml");
    std::fs::write(&out_path, yaml)?;
    println!("wrote {}", out_path.display());
    Ok(())
}

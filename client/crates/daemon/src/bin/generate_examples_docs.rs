//! Regenerates `installer/getting-started.json` from the adjacent
//! `portzero-examples` checkout. Run via `just examples-docs`.
//!
//! The user-facing examples writeup generated from this same data lives at
//! https://portzero.net/docs/examples (source: portzero-cloud's
//! `cloud/landing/blog/src/data/docs/examples.md`), not in this repo.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

const REPO_URL: &str = "https://github.com/PortZeroNetwork/portzero-examples.git";

const LANGUAGES: &[Language] = &[
    Language::new("typescript", "TypeScript", &["node", "nodejs", "npm"]),
    Language::new("javascript", "JavaScript", &["node", "nodejs"]),
    Language::new("python", "Python", &["python3", "python"]),
    Language::new("java", "Java", &["java", "javac"]),
    Language::new("go", "Go", &["go"]),
    Language::new("rust", "Rust", &["cargo", "rustc"]),
    Language::new("csharp", "C#", &["dotnet"]),
    Language::new("php", "PHP", &["php", "composer"]),
    Language::new("ruby", "Ruby", &["ruby", "bundle"]),
    Language::new("cpp", "C/C++", &["cc", "gcc", "clang", "g++", "clang++"]),
    Language::new("swift", "Swift", &["swift"]),
    Language::new("kotlin", "Kotlin", &["kotlin", "kotlinc"]),
    Language::new("dart", "Dart", &["dart"]),
    Language::new("elixir", "Elixir", &["elixir", "mix"]),
    Language::new("scala", "Scala", &["scala", "sbt"]),
    Language::new("r", "R", &["R", "Rscript"]),
    Language::new("julia", "Julia", &["julia"]),
    Language::new("lua", "Lua", &["lua", "luajit"]),
    Language::new("perl", "Perl", &["perl"]),
    Language::new("zig", "Zig", &["zig"]),
    Language::new("haskell", "Haskell", &["ghc", "cabal", "stack"]),
    Language::new("shell", "Shell", &["bash", "sh"]),
];

#[derive(Clone, Copy, Serialize)]
struct Language {
    id: &'static str,
    label: &'static str,
    tools: &'static [&'static str],
}

impl Language {
    const fn new(id: &'static str, label: &'static str, tools: &'static [&'static str]) -> Self {
        Self { id, label, tools }
    }
}

#[derive(Clone)]
struct Example {
    source_language: String,
    language: String,
    variant: String,
    path: PathBuf,
    uses_docker_compose: bool,
}

#[derive(Serialize)]
struct Manifest {
    source: Source,
    languages: Vec<Language>,
    examples: Vec<ExampleRecord>,
}

#[derive(Serialize)]
struct Source {
    repo: &'static str,
    path: &'static str,
    commit: Option<String>,
}

#[derive(Serialize)]
struct ExampleRecord {
    id: String,
    title: String,
    source_language: String,
    language: String,
    language_label: String,
    tools: Vec<&'static str>,
    variant: String,
    variant_label: String,
    requires_docker: bool,
    uses_docker_compose: bool,
    path: String,
    domain: String,
    scripts: PlatformMap,
    commands: PlatformMap,
}

#[derive(Serialize)]
struct PlatformMap {
    linux: String,
    macos: String,
    windows: String,
}

fn main() -> Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .context("resolve repository root")?;
    let examples_root = repo_root
        .join("../portzero-examples")
        .canonicalize()
        .context("resolve ../portzero-examples; clone it next to portzero before running this")?;

    let examples = discover_examples(&examples_root)?;
    anyhow::ensure!(
        !examples.is_empty(),
        "no examples found in {}",
        examples_root.display()
    );

    let manifest = Manifest {
        source: Source {
            repo: REPO_URL,
            path: "../portzero-examples",
            commit: git_commit(&examples_root),
        },
        languages: LANGUAGES.to_vec(),
        examples: examples.iter().map(example_record).collect(),
    };

    let manifest_path = repo_root.join("installer/getting-started.json");
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, format!("{manifest_json}\n"))
        .with_context(|| format!("write {}", manifest_path.display()))?;
    println!("wrote {}", manifest_path.display());

    Ok(())
}

fn discover_examples(root: &Path) -> Result<Vec<Example>> {
    let mut language_dirs = std::fs::read_dir(root)
        .with_context(|| format!("read {}", root.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    language_dirs.sort_by_key(|entry| language_rank(&entry.file_name().to_string_lossy()));

    let mut examples = Vec::new();
    for language_entry in language_dirs {
        let language_path = language_entry.path();
        let language = language_entry.file_name().to_string_lossy().into_owned();
        if !language_path.is_dir()
            || language.starts_with('.')
            || matches!(language.as_str(), "scripts" | "target")
        {
            continue;
        }

        let mut variant_dirs = std::fs::read_dir(&language_path)
            .with_context(|| format!("read {}", language_path.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        variant_dirs.sort_by_key(|entry| variant_rank(&entry.file_name().to_string_lossy()));

        for variant_entry in variant_dirs {
            let variant_path = variant_entry.path();
            let variant = variant_entry.file_name().to_string_lossy().into_owned();
            if !variant_path.is_dir()
                || variant.starts_with('.')
                || !VARIANTS.contains(&variant.as_str())
                || detect_kind(&variant_path).is_none()
            {
                continue;
            }
            examples.push(Example {
                source_language: language.clone(),
                language: language_id_for_dir(&language).to_string(),
                variant,
                uses_docker_compose: variant_path.join("docker-compose.yml").is_file()
                    || variant_path.join("docker-compose.yaml").is_file()
                    || variant_path.join("compose.yaml").is_file(),
                path: variant_path
                    .strip_prefix(root)
                    .unwrap_or(&variant_path)
                    .to_path_buf(),
            });
        }
    }

    Ok(examples)
}

fn language_rank(language: &str) -> (usize, String) {
    let language = language_id_for_dir(language);
    let rank = LANGUAGES
        .iter()
        .position(|item| item.id == language)
        .unwrap_or(LANGUAGES.len());
    (rank, language.to_string())
}

fn language_id_for_dir(dir: &str) -> &str {
    match dir {
        "nodejs-typescript" => "typescript",
        other => other,
    }
}

fn variant_rank(variant: &str) -> (usize, String) {
    let rank = match variant {
        "process" => 0,
        "docker" => 1,
        _ => 2,
    };
    (rank, variant.to_string())
}

fn detect_kind(path: &Path) -> Option<String> {
    // Mirrors logic in ../portzero-examples/scripts/test_examples.py
    if path.join("Cargo.toml").is_file() {
        return Some("rust".to_string());
    }
    if path.join("app.py").is_file() {
        return Some("python".to_string());
    }
    if path.join("app.ts").is_file() {
        return Some("nodejs-typescript".to_string());
    }
    // C# project: any *.csproj
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "csproj") {
                return Some("csharp".to_string());
            }
        }
    }
    None
}

const VARIANTS: &[&str] = &["process", "docker"];

fn example_record(example: &Example) -> ExampleRecord {
    let path = slash_path(&example.path);
    let domain = domain(example);
    let (cmd_part, win_cmd_part) = launch_parts(&example.source_language, &example.variant);
    let unix_command = format!("PZ_TUNNEL=\"{domain}\" {cmd_part}");
    let windows_command = format!("$env:PZ_TUNNEL = \"{domain}\"; {win_cmd_part}");
    let language = language(example.language.as_str());

    let script_file = primary_script_file(&example.source_language, &example.variant);
    let script_linux = if script_file.is_empty() {
        String::new()
    } else {
        format!("{path}/{script_file}")
    };
    let script_windows = script_linux.replace('/', "\\");

    ExampleRecord {
        id: format!("{}/{}", example.source_language, example.variant),
        title: title(example),
        source_language: example.source_language.clone(),
        language: example.language.clone(),
        language_label: language
            .map(|item| item.label)
            .unwrap_or(example.language.as_str())
            .to_string(),
        tools: language.map(|item| item.tools.to_vec()).unwrap_or_default(),
        variant: example.variant.clone(),
        variant_label: variant_label(&example.variant).to_string(),
        requires_docker: example.variant == "docker",
        uses_docker_compose: example.uses_docker_compose,
        path: path.clone(),
        domain: domain.clone(),
        scripts: PlatformMap {
            linux: script_linux.clone(),
            macos: script_linux,
            windows: script_windows,
        },
        commands: PlatformMap {
            linux: unix_command.clone(),
            macos: unix_command,
            windows: windows_command,
        },
    }
}

fn language(id: &str) -> Option<Language> {
    LANGUAGES.iter().copied().find(|item| item.id == id)
}

fn title(example: &Example) -> String {
    let language = language(example.language.as_str())
        .map(|item| item.label)
        .unwrap_or(example.language.as_str());
    format!("{language} {}", variant_label(&example.variant))
}

fn variant_label(variant: &str) -> &str {
    match variant {
        "process" => "process",
        "docker" => "Docker Compose",
        _ => variant,
    }
}

fn launch_parts(source_language: &str, variant: &str) -> (String, String) {
    // Returns (unix_invocation, windows_invocation) — the part after setting PZ_TUNNEL.
    // Matches the documented commands in ../portzero-examples/*/README.md
    if variant == "docker" {
        let cmd = "docker compose up --build".to_string();
        return (cmd.clone(), cmd);
    }
    match source_language {
        "python" => {
            let cmd = "uv run python app.py".to_string();
            (cmd.clone(), cmd)
        }
        "nodejs-typescript" => {
            let cmd = "node app.ts".to_string();
            (cmd.clone(), cmd)
        }
        "rust" => {
            let cmd = "cargo run --quiet".to_string();
            (cmd.clone(), cmd)
        }
        "csharp" => {
            let cmd = "dotnet run --no-launch-profile".to_string();
            (cmd.clone(), cmd)
        }
        _ => (String::new(), String::new()),
    }
}

fn primary_script_file(source_language: &str, variant: &str) -> &'static str {
    if variant == "docker" {
        return "docker-compose.yml";
    }
    match source_language {
        "python" => "app.py",
        "nodejs-typescript" => "app.ts",
        "rust" => "src/main.rs",
        "csharp" => "Program.cs",
        _ => "",
    }
}

fn domain(example: &Example) -> String {
    format!(
        "{}-{}.portzero.local:80",
        example.source_language, example.variant
    )
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn git_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

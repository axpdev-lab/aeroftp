//! Context Intelligence — project detection, import scanning, git context, agent memory
//!
//! Provides 5 Tauri commands for AeroAgent context awareness:
//! - `detect_project_context`: Detect project type, scripts, dependencies
//! - `scan_file_imports`: Parse imports/requires/uses from source files
//! - `get_git_context`: Git branch, recent commits, uncommitted changes
//! - `read_agent_memory`: Read persistent agent memory for a project
//! - `write_agent_memory`: Append entries to agent memory file

use serde::{Serialize, Deserialize};
use std::path::{Path, PathBuf, Component};
use std::sync::LazyLock;
use regex::Regex;

// ─── Shared path validation ────────────────────────────────────────────────

/// Validate a path used by context intelligence commands.
/// Rejects null bytes, `..` traversal components, and excessive length.
/// Does NOT require canonicalization to $HOME (these commands work on any project directory).
fn validate_context_path(path: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err("Path exceeds 4096 character limit".to_string());
    }
    if path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err("Path traversal ('..') not allowed".to_string());
        }
    }
    Ok(())
}

// ─── Write mutex ────────────────────────────────────────────────────────────

static MEMORY_WRITE_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

// ─── LazyLock regex patterns ────────────────────────────────────────────────

// Rust Cargo.toml [[bin]] name
static CARGO_BIN_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"name\s*=\s*"([^"]+)""#).expect("CARGO_BIN_NAME_RE")
});

// Java Maven pom.xml
static MAVEN_ARTIFACT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<artifactId>([^<]+)</artifactId>").expect("MAVEN_ARTIFACT_RE")
});
static MAVEN_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<version>([^<]+)</version>").expect("MAVEN_VERSION_RE")
});

// JS/TS import patterns
static JS_IMPORT_FROM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+.*?\s+from\s+['"]([^'"]+)['"]"#).expect("JS_IMPORT_FROM_RE")
});
static JS_IMPORT_SIDE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+['"]([^'"]+)['"]"#).expect("JS_IMPORT_SIDE_RE")
});
static JS_REQUIRE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"require\(\s*['"]([^'"]+)['"]\s*\)"#).expect("JS_REQUIRE_RE")
});
static JS_DYNAMIC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\(\s*['"]([^'"]+)['"]\s*\)"#).expect("JS_DYNAMIC_RE")
});

// Rust import patterns
static RUST_USE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*use\s+([^;]+);").expect("RUST_USE_RE")
});
static RUST_MOD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*mod\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*;").expect("RUST_MOD_RE")
});

// Python import patterns
static PY_FROM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*from\s+([^\s]+)\s+import").expect("PY_FROM_RE")
});
static PY_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*import\s+([^\s,]+)").expect("PY_IMPORT_RE")
});

// PHP import patterns
static PHP_USE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*use\s+([^;]+);").expect("PHP_USE_RE")
});
static PHP_REQUIRE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:require_once|require|include_once|include)\s+['"]([^'"]+)['"]"#).expect("PHP_REQUIRE_RE")
});

// Go import patterns
static GO_SINGLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*import\s+"([^"]+)""#).expect("GO_SINGLE_RE")
});
static GO_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*"([^"]+)""#).expect("GO_BLOCK_RE")
});

// Java import pattern
static JAVA_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*import\s+(static\s+)?([^;]+);").expect("JAVA_IMPORT_RE")
});

/// Read a config file only if it's under 5MB. Returns None if file is too large or unreadable.
fn read_config_file(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > 5 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

// ─── Structs ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct ProjectContext {
    pub project_type: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub scripts: Vec<String>,
    pub deps_count: u32,
    pub dev_deps_count: u32,
    pub entry_points: Vec<String>,
    pub config_files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ImportEntry {
    pub source: String,
    pub resolved_path: Option<String>,
    pub kind: String,
}

#[derive(Serialize, Deserialize)]
pub struct GitCommit {
    pub hash: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
pub struct GitContext {
    pub branch: String,
    pub recent_commits: Vec<GitCommit>,
    pub uncommitted_changes: Vec<String>,
    pub has_uncommitted: bool,
}

// ─── Command 1: detect_project_context ──────────────────────────────────────

#[tauri::command]
pub async fn detect_project_context(path: String) -> Result<ProjectContext, String> {
    validate_context_path(&path)?;
    let base = Path::new(&path);
    if !base.exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    let mut ctx = ProjectContext {
        project_type: "unknown".to_string(),
        name: None,
        version: None,
        scripts: Vec::new(),
        deps_count: 0,
        dev_deps_count: 0,
        entry_points: Vec::new(),
        config_files: Vec::new(),
    };

    // Scan for config files
    let config_patterns: &[&str] = &[
        ".eslintrc", ".eslintrc.js", ".eslintrc.json", ".eslintrc.yml",
        "tsconfig.json", ".prettierrc", ".prettierrc.js", ".prettierrc.json",
        "jest.config.js", "jest.config.ts", "jest.config.mjs",
        "vite.config.js", "vite.config.ts", "vite.config.mjs",
        "webpack.config.js", "webpack.config.ts",
        "Dockerfile", ".env.example", ".gitignore",
        "tailwind.config.js", "tailwind.config.ts",
        "next.config.js", "next.config.mjs",
        "nuxt.config.ts", "svelte.config.js",
    ];

    for pattern in config_patterns {
        if base.join(pattern).exists() {
            ctx.config_files.push(pattern.to_string());
        }
    }

    // Check for CI/CD
    if base.join(".github/workflows").is_dir() {
        ctx.config_files.push(".github/workflows".to_string());
    }
    if base.join(".gitlab-ci.yml").exists() {
        ctx.config_files.push(".gitlab-ci.yml".to_string());
    }

    // Try detecting project type in order of specificity
    let detected = try_detect_nodejs(base, &mut ctx)
        || try_detect_rust(base, &mut ctx)
        || try_detect_python(base, &mut ctx)
        || try_detect_php(base, &mut ctx)
        || try_detect_go(base, &mut ctx)
        || try_detect_java(base, &mut ctx)
        || try_detect_dotnet(base, &mut ctx);

    if detected {
        // project type already set by the detection function
    } else if base.join("Makefile").exists() || base.join("CMakeLists.txt").exists() {
        ctx.project_type = "c_cpp".to_string();
        if base.join("Makefile").exists() {
            ctx.config_files.push("Makefile".to_string());
        }
        if base.join("CMakeLists.txt").exists() {
            ctx.config_files.push("CMakeLists.txt".to_string());
        }
    } else {
        // Try one level up for monorepo detection
        if let Some(parent) = base.parent() {
            if parent.join("package.json").exists() {
                try_detect_nodejs(parent, &mut ctx);
                ctx.project_type = format!("{}/sub", ctx.project_type);
            } else if parent.join("Cargo.toml").exists() {
                try_detect_rust(parent, &mut ctx);
                ctx.project_type = format!("{}/sub", ctx.project_type);
            }
        }
    }

    Ok(ctx)
}

fn try_detect_nodejs(base: &Path, ctx: &mut ProjectContext) -> bool {
    let pkg_path = base.join("package.json");
    if !pkg_path.exists() {
        return false;
    }

    ctx.project_type = "nodejs".to_string();
    ctx.config_files.push("package.json".to_string());

    if let Some(content) = read_config_file(&pkg_path) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            ctx.name = pkg.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
            ctx.version = pkg.get("version").and_then(|v| v.as_str()).map(|s| s.to_string());

            // Scripts
            if let Some(scripts) = pkg.get("scripts").and_then(|v| v.as_object()) {
                ctx.scripts = scripts.keys().cloned().collect();
            }

            // Dependencies count
            if let Some(deps) = pkg.get("dependencies").and_then(|v| v.as_object()) {
                ctx.deps_count = deps.len() as u32;
            }
            if let Some(dev_deps) = pkg.get("devDependencies").and_then(|v| v.as_object()) {
                ctx.dev_deps_count = dev_deps.len() as u32;
            }

            // Entry points
            if let Some(main) = pkg.get("main").and_then(|v| v.as_str()) {
                ctx.entry_points.push(main.to_string());
            }
            if let Some(module) = pkg.get("module").and_then(|v| v.as_str()) {
                ctx.entry_points.push(module.to_string());
            }
            if let Some(bin) = pkg.get("bin") {
                if let Some(bin_str) = bin.as_str() {
                    ctx.entry_points.push(bin_str.to_string());
                } else if let Some(bin_obj) = bin.as_object() {
                    for val in bin_obj.values() {
                        if let Some(s) = val.as_str() {
                            ctx.entry_points.push(s.to_string());
                        }
                    }
                }
            }
        }
    }

    // Detect lock files
    for lock in &["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb"] {
        if base.join(lock).exists() {
            ctx.config_files.push(lock.to_string());
        }
    }

    true
}

fn try_detect_rust(base: &Path, ctx: &mut ProjectContext) -> bool {
    let cargo_path = base.join("Cargo.toml");
    if !cargo_path.exists() {
        return false;
    }

    ctx.project_type = "rust".to_string();
    ctx.config_files.push("Cargo.toml".to_string());

    if let Some(content) = read_config_file(&cargo_path) {
        // Parse [package] section
        let mut in_package = false;
        let mut in_dependencies = false;
        let mut in_dev_dependencies = false;
        let mut deps: u32 = 0;
        let mut dev_deps: u32 = 0;

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with('[') {
                in_package = trimmed == "[package]";
                in_dependencies = trimmed == "[dependencies]";
                in_dev_dependencies = trimmed == "[dev-dependencies]";
                continue;
            }

            if in_package {
                if let Some(val) = parse_toml_value(trimmed, "name") {
                    ctx.name = Some(val);
                }
                if let Some(val) = parse_toml_value(trimmed, "version") {
                    ctx.version = Some(val);
                }
            }

            if in_dependencies && trimmed.contains('=') && !trimmed.starts_with('#') {
                deps += 1;
            }
            if in_dev_dependencies && trimmed.contains('=') && !trimmed.starts_with('#') {
                dev_deps += 1;
            }
        }

        ctx.deps_count = deps;
        ctx.dev_deps_count = dev_deps;

        // Entry points: look for [[bin]] entries
        let mut in_bin = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[[bin]]" {
                in_bin = true;
                continue;
            }
            if trimmed.starts_with('[') && trimmed != "[[bin]]" {
                in_bin = false;
            }
            if in_bin {
                if let Some(caps) = CARGO_BIN_NAME_RE.captures(trimmed) {
                    ctx.entry_points.push(caps[1].to_string());
                }
            }
        }

        // Default entry point
        if base.join("src/main.rs").exists() {
            ctx.entry_points.push("src/main.rs".to_string());
        }
        if base.join("src/lib.rs").exists() {
            ctx.entry_points.push("src/lib.rs".to_string());
        }
    }

    // Scripts equivalent for Rust
    if base.join("Makefile").exists() {
        ctx.scripts.push("make".to_string());
    }
    if base.join("justfile").exists() {
        ctx.scripts.push("just".to_string());
    }
    ctx.scripts.extend_from_slice(&[
        "cargo build".to_string(),
        "cargo test".to_string(),
        "cargo check".to_string(),
    ]);

    if base.join("Cargo.lock").exists() {
        ctx.config_files.push("Cargo.lock".to_string());
    }
    if base.join("rust-toolchain.toml").exists() {
        ctx.config_files.push("rust-toolchain.toml".to_string());
    }
    if base.join("clippy.toml").exists() {
        ctx.config_files.push("clippy.toml".to_string());
    }

    true
}

fn try_detect_python(base: &Path, ctx: &mut ProjectContext) -> bool {
    let pyproject = base.join("pyproject.toml");
    let requirements = base.join("requirements.txt");
    let setup_py = base.join("setup.py");

    if !pyproject.exists() && !requirements.exists() && !setup_py.exists() {
        return false;
    }

    ctx.project_type = "python".to_string();

    if pyproject.exists() {
        ctx.config_files.push("pyproject.toml".to_string());
        if let Some(content) = read_config_file(&pyproject) {
            // Basic TOML parsing for [project] section
            let mut in_project = false;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_project = trimmed == "[project]" || trimmed == "[tool.poetry]";
                    continue;
                }
                if in_project {
                    if let Some(val) = parse_toml_value(trimmed, "name") {
                        ctx.name = Some(val);
                    }
                    if let Some(val) = parse_toml_value(trimmed, "version") {
                        ctx.version = Some(val);
                    }
                }
            }

            // Count dependencies (rough: lines under [project.dependencies] or [tool.poetry.dependencies])
            let mut in_deps = false;
            let mut count: u32 = 0;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_deps = trimmed.contains("dependencies") && !trimmed.contains("dev");
                    continue;
                }
                if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
                    count += 1;
                }
            }
            ctx.deps_count = count;
        }
    }

    if requirements.exists() {
        ctx.config_files.push("requirements.txt".to_string());
        if ctx.deps_count == 0 {
            if let Some(content) = read_config_file(&requirements) {
                ctx.deps_count = content.lines()
                    .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
                    .count() as u32;
            }
        }
    }

    if setup_py.exists() {
        ctx.config_files.push("setup.py".to_string());
    }

    // Entry points
    if base.join("main.py").exists() {
        ctx.entry_points.push("main.py".to_string());
    }
    if base.join("app.py").exists() {
        ctx.entry_points.push("app.py".to_string());
    }
    if base.join("manage.py").exists() {
        ctx.entry_points.push("manage.py".to_string());
        ctx.scripts.push("python manage.py".to_string());
    }

    // Virtual env detection
    for venv in &["venv", ".venv", "env"] {
        if base.join(venv).is_dir() {
            ctx.config_files.push(venv.to_string());
        }
    }

    true
}

fn try_detect_php(base: &Path, ctx: &mut ProjectContext) -> bool {
    let composer = base.join("composer.json");
    if !composer.exists() {
        return false;
    }

    ctx.project_type = "php".to_string();
    ctx.config_files.push("composer.json".to_string());

    if let Some(content) = read_config_file(&composer) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            ctx.name = pkg.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
            ctx.version = pkg.get("version").and_then(|v| v.as_str()).map(|s| s.to_string());

            if let Some(req) = pkg.get("require").and_then(|v| v.as_object()) {
                ctx.deps_count = req.len() as u32;
            }
            if let Some(req_dev) = pkg.get("require-dev").and_then(|v| v.as_object()) {
                ctx.dev_deps_count = req_dev.len() as u32;
            }
        }
    }

    if base.join("artisan").exists() {
        ctx.entry_points.push("artisan".to_string());
        ctx.scripts.push("php artisan".to_string());
    }
    if base.join("index.php").exists() {
        ctx.entry_points.push("index.php".to_string());
    }

    true
}

fn try_detect_go(base: &Path, ctx: &mut ProjectContext) -> bool {
    let gomod = base.join("go.mod");
    if !gomod.exists() {
        return false;
    }

    ctx.project_type = "go".to_string();
    ctx.config_files.push("go.mod".to_string());

    if let Some(content) = read_config_file(&gomod) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("module ") {
                ctx.name = Some(trimmed.trim_start_matches("module ").trim().to_string());
            }
            if trimmed.starts_with("go ") {
                ctx.version = Some(trimmed.trim_start_matches("go ").trim().to_string());
            }
        }

        // Count require lines
        let mut in_require = false;
        let mut count: u32 = 0;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "require (" {
                in_require = true;
                continue;
            }
            if trimmed == ")" && in_require {
                in_require = false;
                continue;
            }
            if in_require && !trimmed.is_empty() && !trimmed.starts_with("//") {
                count += 1;
            }
            // Single-line require
            if trimmed.starts_with("require ") && !trimmed.contains('(') {
                count += 1;
            }
        }
        ctx.deps_count = count;
    }

    // Entry points
    if base.join("main.go").exists() {
        ctx.entry_points.push("main.go".to_string());
    }
    if base.join("cmd").is_dir() {
        ctx.entry_points.push("cmd/".to_string());
    }

    if base.join("go.sum").exists() {
        ctx.config_files.push("go.sum".to_string());
    }

    ctx.scripts.extend_from_slice(&[
        "go build".to_string(),
        "go test ./...".to_string(),
    ]);

    true
}

fn try_detect_java(base: &Path, ctx: &mut ProjectContext) -> bool {
    let pom = base.join("pom.xml");
    let gradle = base.join("build.gradle");
    let gradle_kts = base.join("build.gradle.kts");

    if !pom.exists() && !gradle.exists() && !gradle_kts.exists() {
        return false;
    }

    if pom.exists() {
        ctx.project_type = "java_maven".to_string();
        ctx.config_files.push("pom.xml".to_string());

        if let Some(content) = read_config_file(&pom) {
            // Get first artifactId (project-level)
            if let Some(caps) = MAVEN_ARTIFACT_RE.captures(&content) {
                ctx.name = Some(caps[1].to_string());
            }
            if let Some(caps) = MAVEN_VERSION_RE.captures(&content) {
                ctx.version = Some(caps[1].to_string());
            }

            // Count <dependency> elements
            ctx.deps_count = content.matches("<dependency>").count() as u32;
        }

        ctx.scripts.extend_from_slice(&[
            "mvn compile".to_string(),
            "mvn test".to_string(),
            "mvn package".to_string(),
        ]);
    } else {
        ctx.project_type = "java_gradle".to_string();
        if gradle.exists() {
            ctx.config_files.push("build.gradle".to_string());
        }
        if gradle_kts.exists() {
            ctx.config_files.push("build.gradle.kts".to_string());
        }

        ctx.scripts.extend_from_slice(&[
            "gradle build".to_string(),
            "gradle test".to_string(),
        ]);
    }

    true
}

fn try_detect_dotnet(base: &Path, ctx: &mut ProjectContext) -> bool {
    // Check for .sln files
    let has_sln = std::fs::read_dir(base)
        .map(|entries| {
            entries.filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|ext| ext == "sln"))
        })
        .unwrap_or(false);

    let has_csproj = std::fs::read_dir(base)
        .map(|entries| {
            entries.filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|ext| ext == "csproj"))
        })
        .unwrap_or(false);

    if !has_sln && !has_csproj {
        return false;
    }

    ctx.project_type = "dotnet".to_string();

    if has_sln {
        ctx.config_files.push("*.sln".to_string());
    }
    if has_csproj {
        ctx.config_files.push("*.csproj".to_string());
    }

    ctx.scripts.extend_from_slice(&[
        "dotnet build".to_string(),
        "dotnet test".to_string(),
        "dotnet run".to_string(),
    ]);

    true
}

/// Parse a simple TOML key = "value" line
fn parse_toml_value(line: &str, key: &str) -> Option<String> {
    let trimmed = line.trim();
    let after_key = match trimmed.strip_prefix(key) {
        Some(rest) => rest.trim_start(),
        None => return None,
    };
    let after_eq = match after_key.strip_prefix('=') {
        Some(rest) => rest.trim_start(),
        None => return None,
    };
    let inner = after_eq.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

// ─── Command 2: scan_file_imports ───────────────────────────────────────────

#[tauri::command]
pub async fn scan_file_imports(path: String) -> Result<Vec<ImportEntry>, String> {
    validate_context_path(&path)?;
    let file_path = Path::new(&path);
    if !file_path.exists() {
        return Err(format!("File does not exist: {}", path));
    }

    // FIX 3: Check extension BEFORE reading the file
    let ext = file_path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    const SUPPORTED_EXTENSIONS: &[&str] = &[
        "js", "jsx", "ts", "tsx", "mjs", "mts", "rs", "py", "php", "go", "java",
    ];
    if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return Ok(Vec::new());
    }

    // FIX 2: Check file size via metadata BEFORE reading into memory
    let metadata = std::fs::metadata(&path)
        .map_err(|e| format!("Failed to stat file: {}", e))?;
    if metadata.len() > 1_048_576 {
        return Err("File too large for import scanning (max 1MB)".to_string());
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let parent = file_path.parent().unwrap_or(Path::new("."));

    let mut imports = match ext.as_str() {
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "mts" => scan_js_imports(&content, parent),
        "rs" => scan_rust_imports(&content, parent),
        "py" => scan_python_imports(&content),
        "php" => scan_php_imports(&content, parent),
        "go" => scan_go_imports(&content),
        "java" => scan_java_imports(&content),
        _ => Vec::new(),
    };

    imports.truncate(30);
    Ok(imports)
}

fn scan_js_imports(content: &str, parent: &Path) -> Vec<ImportEntry> {
    let mut imports = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for caps in JS_IMPORT_FROM_RE.captures_iter(content) {
        let source = caps[1].to_string();
        if seen.insert(source.clone()) {
            let resolved = resolve_js_import(&source, parent);
            imports.push(ImportEntry { source, resolved_path: resolved, kind: "import".to_string() });
        }
    }

    for caps in JS_IMPORT_SIDE_RE.captures_iter(content) {
        let source = caps[1].to_string();
        // Skip if already captured by JS_IMPORT_FROM_RE (HashSet dedup)
        if seen.insert(source.clone()) {
            let resolved = resolve_js_import(&source, parent);
            imports.push(ImportEntry { source, resolved_path: resolved, kind: "import".to_string() });
        }
    }

    for caps in JS_REQUIRE_RE.captures_iter(content) {
        let source = caps[1].to_string();
        if seen.insert(source.clone()) {
            let resolved = resolve_js_import(&source, parent);
            imports.push(ImportEntry { source, resolved_path: resolved, kind: "require".to_string() });
        }
    }

    for caps in JS_DYNAMIC_RE.captures_iter(content) {
        let source = caps[1].to_string();
        if seen.insert(source.clone()) {
            let resolved = resolve_js_import(&source, parent);
            imports.push(ImportEntry { source, resolved_path: resolved, kind: "import".to_string() });
        }
    }

    imports
}

fn resolve_js_import(source: &str, parent: &Path) -> Option<String> {
    if !source.starts_with('.') {
        return None; // node_module — not resolvable locally
    }

    let base = parent.join(source);

    // Try exact path first
    if base.exists() && base.is_file() {
        return Some(base.to_string_lossy().to_string());
    }

    // Try common extensions
    let extensions = [".ts", ".tsx", ".js", ".jsx", ".mjs"];
    for ext in &extensions {
        let with_ext = PathBuf::from(format!("{}{}", base.display(), ext));
        if with_ext.exists() {
            return Some(with_ext.to_string_lossy().to_string());
        }
    }

    // Try index files
    let index_names = ["index.ts", "index.tsx", "index.js", "index.jsx"];
    for idx in &index_names {
        let index_path = base.join(idx);
        if index_path.exists() {
            return Some(index_path.to_string_lossy().to_string());
        }
    }

    None
}

fn scan_rust_imports(content: &str, parent: &Path) -> Vec<ImportEntry> {
    let mut imports = Vec::new();

    for line in content.lines() {
        if let Some(caps) = RUST_USE_RE.captures(line) {
            let source = caps[1].trim().to_string();
            imports.push(ImportEntry { source, resolved_path: None, kind: "use".to_string() });
        }
        if let Some(caps) = RUST_MOD_RE.captures(line) {
            let mod_name = caps[1].to_string();
            let resolved = resolve_rust_mod(&mod_name, parent);
            imports.push(ImportEntry { source: mod_name, resolved_path: resolved, kind: "mod".to_string() });
        }
    }

    imports
}

fn resolve_rust_mod(mod_name: &str, parent: &Path) -> Option<String> {
    // mod foo; → foo.rs or foo/mod.rs
    let file_path = parent.join(format!("{}.rs", mod_name));
    if file_path.exists() {
        return Some(file_path.to_string_lossy().to_string());
    }
    let dir_path = parent.join(mod_name).join("mod.rs");
    if dir_path.exists() {
        return Some(dir_path.to_string_lossy().to_string());
    }
    None
}

fn scan_python_imports(content: &str) -> Vec<ImportEntry> {
    let mut imports = Vec::new();

    for line in content.lines() {
        if let Some(caps) = PY_FROM_RE.captures(line) {
            let source = caps[1].to_string();
            imports.push(ImportEntry { source, resolved_path: None, kind: "from".to_string() });
        } else if let Some(caps) = PY_IMPORT_RE.captures(line) {
            let source = caps[1].to_string();
            imports.push(ImportEntry { source, resolved_path: None, kind: "import".to_string() });
        }
    }

    imports
}

fn scan_php_imports(content: &str, parent: &Path) -> Vec<ImportEntry> {
    let mut imports = Vec::new();

    for line in content.lines() {
        if let Some(caps) = PHP_USE_RE.captures(line) {
            let source = caps[1].trim().to_string();
            imports.push(ImportEntry { source, resolved_path: None, kind: "use".to_string() });
        }
    }

    for caps in PHP_REQUIRE_RE.captures_iter(content) {
        let source = caps[1].to_string();
        let resolved = if source.starts_with('.') || source.starts_with('/') {
            let p = parent.join(&source);
            if p.exists() { Some(p.to_string_lossy().to_string()) } else { None }
        } else {
            None
        };
        imports.push(ImportEntry { source, resolved_path: resolved, kind: "include".to_string() });
    }

    imports
}

fn scan_go_imports(content: &str) -> Vec<ImportEntry> {
    let mut imports = Vec::new();

    let mut in_import_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("import (") {
            in_import_block = true;
            continue;
        }
        if in_import_block && trimmed == ")" {
            in_import_block = false;
            continue;
        }

        if in_import_block {
            if let Some(caps) = GO_BLOCK_RE.captures(trimmed) {
                let source = caps[1].to_string();
                imports.push(ImportEntry { source, resolved_path: None, kind: "import".to_string() });
            }
        } else if let Some(caps) = GO_SINGLE_RE.captures(line) {
            let source = caps[1].to_string();
            imports.push(ImportEntry { source, resolved_path: None, kind: "import".to_string() });
        }
    }

    imports
}

fn scan_java_imports(content: &str) -> Vec<ImportEntry> {
    let mut imports = Vec::new();

    for line in content.lines() {
        if let Some(caps) = JAVA_IMPORT_RE.captures(line) {
            let source = caps[2].trim().to_string();
            imports.push(ImportEntry { source, resolved_path: None, kind: "import".to_string() });
        }
    }

    imports
}

// ─── Command 3: get_git_context ─────────────────────────────────────────────

#[tauri::command]
pub async fn get_git_context(path: String) -> Result<GitContext, String> {
    validate_context_path(&path)?;
    let base = Path::new(&path);
    if !base.exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    // Check if it's a git repo
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&path)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git is not installed or not found in PATH".to_string()
            } else {
                format!("Failed to run git: {}", e)
            }
        })?;

    if !output.status.success() {
        return Err(format!("Not a git repository: {}", path));
    }

    // Get current branch
    let branch_output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&path)
        .output()
        .map_err(|e| format!("Failed to get branch: {}", e))?;

    let branch = String::from_utf8_lossy(&branch_output.stdout).trim().to_string();

    // Get recent commits
    let log_output = std::process::Command::new("git")
        .args(["log", "--oneline", "-10", "--no-color"])
        .current_dir(&path)
        .output()
        .map_err(|e| format!("Failed to get git log: {}", e))?;

    let log_text = String::from_utf8_lossy(&log_output.stdout);
    let recent_commits: Vec<GitCommit> = log_text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            GitCommit {
                hash: parts.first().unwrap_or(&"").to_string(),
                message: parts.get(1).unwrap_or(&"").to_string(),
            }
        })
        .collect();

    // Get uncommitted changes
    let status_output = std::process::Command::new("git")
        .args(["status", "--short", "--no-color"])
        .current_dir(&path)
        .output()
        .map_err(|e| format!("Failed to get git status: {}", e))?;

    let status_text = String::from_utf8_lossy(&status_output.stdout);
    let all_changes: Vec<String> = status_text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    let has_uncommitted = !all_changes.is_empty();
    let total_changes = all_changes.len();
    let mut uncommitted_changes: Vec<String> = all_changes.into_iter().take(100).collect();
    if total_changes > 100 {
        uncommitted_changes.push(format!("...and {} more", total_changes - 100));
    }

    Ok(GitContext {
        branch,
        recent_commits,
        uncommitted_changes,
        has_uncommitted,
    })
}

// ─── Command 4: read_agent_memory ───────────────────────────────────────────

#[tauri::command]
pub async fn read_agent_memory(project_path: String) -> Result<String, String> {
    validate_context_path(&project_path)?;
    let memory_path = Path::new(&project_path).join(".aeroagent");

    if !memory_path.exists() {
        return Ok(String::new());
    }

    let metadata = std::fs::metadata(&memory_path)
        .map_err(|e| format!("Failed to read memory file metadata: {}", e))?;

    if metadata.len() > 50 * 1024 {
        return Err("Agent memory file exceeds 50KB limit".to_string());
    }

    std::fs::read_to_string(&memory_path)
        .map_err(|e| format!("Failed to read agent memory: {}", e))
}

// ─── Command 5: write_agent_memory ──────────────────────────────────────────

#[tauri::command]
pub async fn write_agent_memory(project_path: String, content: String) -> Result<(), String> {
    validate_context_path(&project_path)?;

    // Validate content length
    if content.len() > 5000 {
        return Err("Content exceeds 5000 character limit".to_string());
    }

    // Acquire write lock to prevent TOCTOU race on size check
    let _lock = MEMORY_WRITE_LOCK.lock().await;

    let memory_path = Path::new(&project_path).join(".aeroagent");

    // Check existing file size to ensure total won't exceed 50KB
    if memory_path.exists() {
        let metadata = std::fs::metadata(&memory_path)
            .map_err(|e| format!("Failed to check memory file: {}", e))?;
        if metadata.len() + content.len() as u64 > 50 * 1024 {
            return Err("Writing would exceed 50KB memory file limit".to_string());
        }
    }

    // Append to file (create if not exists)
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&memory_path)
        .map_err(|e| format!("Failed to open memory file: {}", e))?;

    file.write_all(content.as_bytes())
        .map_err(|e| format!("Failed to write agent memory: {}", e))?;

    Ok(())
}

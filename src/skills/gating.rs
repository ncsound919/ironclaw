//! Requirements gating for skills.
//!
//! Checks that a skill's declared requirements (binaries, environment variables,
//! config files, Python packages) are satisfied before the skill is loaded.
//!
//! Optional requirements are checked and logged as warnings, but do not prevent
//! skill loading.

use crate::skills::GatingRequirements;

/// Result of a gating check.
#[derive(Debug)]
pub struct GatingResult {
    /// Whether all requirements passed.
    pub passed: bool,
    /// Descriptions of failed requirements.
    pub failures: Vec<String>,
    /// Descriptions of missing optional requirements (warnings only).
    pub warnings: Vec<String>,
}

/// Async wrapper around [`check_requirements_sync`] that offloads blocking
/// subprocess calls (`which`/`where`) to a blocking thread pool via
/// `tokio::task::spawn_blocking`.
pub async fn check_requirements(requirements: &GatingRequirements) -> GatingResult {
    let requirements = requirements.clone();
    tokio::task::spawn_blocking(move || check_requirements_sync(&requirements))
        .await
        .unwrap_or_else(|e| {
            let message = if e.is_panic() {
                format!("gating check panicked: {}", e)
            } else if e.is_cancelled() {
                format!("gating check task was cancelled: {}", e)
            } else {
                format!("gating check failed to join: {}", e)
            };
            tracing::error!("{}", message);
            GatingResult {
                passed: false,
                failures: vec![message],
                warnings: vec![],
            }
        })
}

/// Check whether gating requirements are satisfied (synchronous).
///
/// - `bins`: checks that each binary is findable via `which` (PATH lookup).
/// - `env`: checks that each environment variable is set.
/// - `config`: checks that each config file path exists.
/// - `python_packages`: checks that each package is installed via `pip list`.
/// - `optional_*`: checked but only warn if missing, do not fail gating.
///
/// Skills that fail gating should be logged and skipped, not loaded.
///
/// This is the synchronous implementation; prefer the async [`check_requirements`]
/// wrapper when calling from async contexts to avoid blocking the tokio runtime.
pub fn check_requirements_sync(requirements: &GatingRequirements) -> GatingResult {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    // Check required binaries
    for bin in &requirements.bins {
        if !binary_exists(bin) {
            failures.push(format!("required binary not found: {}", bin));
        }
    }

    // Check required environment variables
    for var in &requirements.env {
        if std::env::var(var).is_err() {
            failures.push(format!("required env var not set: {}", var));
        }
    }

    // Check required config files
    for path in &requirements.config {
        if !std::path::Path::new(path).exists() {
            failures.push(format!("required config not found: {}", path));
        }
    }

    // Check required Python packages
    for package in &requirements.python_packages {
        if !python_package_exists(package) {
            failures.push(format!(
                "required Python package not installed: {}",
                package
            ));
        }
    }

    // Check optional binaries (warnings only)
    for bin in &requirements.optional_bins {
        if !binary_exists(bin) {
            warnings.push(format!("optional binary not found: {}", bin));
        }
    }

    // Check optional environment variables (warnings only)
    for var in &requirements.optional_env {
        if std::env::var(var).is_err() {
            warnings.push(format!("optional env var not set: {}", var));
        }
    }

    // Check optional config files (warnings only)
    for path in &requirements.optional_config {
        if !std::path::Path::new(path).exists() {
            warnings.push(format!("optional config not found: {}", path));
        }
    }

    GatingResult {
        passed: failures.is_empty(),
        failures,
        warnings,
    }
}

/// Check if a binary exists on PATH using `std::process::Command`.
fn binary_exists(name: &str) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("which")
            .arg(name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("where")
            .arg(name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// Check if a Python package is installed using `python3 -m pip list`.
///
/// This function runs `python3 -m pip list --format=freeze` and searches for
/// the package name in the output. The package name is matched case-insensitively
/// against the beginning of each line (before the `==` version separator).
///
/// Returns `false` if Python is not available or the package is not found.
fn python_package_exists(package_name: &str) -> bool {
    // Try python3 first, fall back to python
    let python_cmd = if binary_exists("python3") {
        "python3"
    } else if binary_exists("python") {
        "python"
    } else {
        // No Python available
        return false;
    };

    let output = match std::process::Command::new(python_cmd)
        .args(["-m", "pip", "list", "--format=freeze"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };

    if !output.status.success() {
        return false;
    }

    let stdout = match std::str::from_utf8(&output.stdout) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Search for package name (case-insensitive) at the start of any line
    // Format is "package-name==version" or "package_name==version"
    let search_name = package_name.to_lowercase();
    stdout.lines().any(|line| {
        let line_lower = line.to_lowercase();
        // Match package name followed by == or end of line
        line_lower.starts_with(&search_name)
            && (line_lower.len() == search_name.len()
                || line_lower[search_name.len()..].starts_with("=="))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_requirements_pass() {
        let req = GatingRequirements::default();
        let result = check_requirements_sync(&req);
        assert!(result.passed);
        assert!(result.failures.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_missing_binary_fails() {
        let req = GatingRequirements {
            bins: vec!["__ironclaw_nonexistent_binary_xyz__".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        assert!(!result.passed);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].contains("binary not found"));
    }

    #[test]
    fn test_missing_env_var_fails() {
        let req = GatingRequirements {
            env: vec!["__IRONCLAW_TEST_NONEXISTENT_VAR__".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        assert!(!result.passed);
        assert!(result.failures[0].contains("env var not set"));
    }

    #[test]
    fn test_present_env_var_passes() {
        // PATH is always set on both Unix and Windows
        let req = GatingRequirements {
            env: vec!["PATH".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        assert!(result.passed);
    }

    #[test]
    fn test_missing_config_fails() {
        let req = GatingRequirements {
            config: vec!["/nonexistent/path/ironclaw_test.conf".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        assert!(!result.passed);
        assert!(result.failures[0].contains("config not found"));
    }

    #[test]
    fn test_multiple_mixed_requirements() {
        let req = GatingRequirements {
            bins: vec!["__no_such_bin__".to_string()],
            env: vec!["__NO_SUCH_VAR__".to_string()],
            config: vec!["/no/such/file".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        assert!(!result.passed);
        assert_eq!(result.failures.len(), 3);
    }

    #[test]
    fn test_optional_requirements_warn_but_pass() {
        let req = GatingRequirements {
            optional_bins: vec!["__no_such_bin__".to_string()],
            optional_env: vec!["__NO_SUCH_VAR__".to_string()],
            optional_config: vec!["/no/such/file".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        // Should pass despite missing optional requirements
        assert!(result.passed);
        assert!(result.failures.is_empty());
        // But should have warnings
        assert_eq!(result.warnings.len(), 3);
        assert!(result.warnings[0].contains("optional binary not found"));
        assert!(result.warnings[1].contains("optional env var not set"));
        assert!(result.warnings[2].contains("optional config not found"));
    }

    #[test]
    fn test_python_package_check() {
        // Test with a nonexistent package
        let req = GatingRequirements {
            python_packages: vec!["__nonexistent_python_package_xyz__".to_string()],
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        // Should fail if Python is available, or pass if Python is not available
        // We can't guarantee Python is installed in test environment
        if binary_exists("python3") || binary_exists("python") {
            assert!(!result.passed);
            assert!(result.failures[0].contains("Python package not installed"));
        }
    }

    #[test]
    fn test_mixed_required_and_optional() {
        let req = GatingRequirements {
            env: vec!["PATH".to_string()], // Required and exists
            optional_env: vec!["__NO_SUCH_VAR__".to_string()], // Optional and missing
            ..Default::default()
        };
        let result = check_requirements_sync(&req);
        assert!(result.passed); // Should pass because required env exists
        assert!(result.failures.is_empty());
        assert_eq!(result.warnings.len(), 1); // Should warn about optional
    }
}

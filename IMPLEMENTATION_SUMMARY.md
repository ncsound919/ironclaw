# Science IDE Capabilities - Implementation Summary

## Overview

This implementation adds comprehensive support for scientific computing capabilities through the OpenClaw metadata system. Five specialized skills have been created to transform IronClaw into a production-grade "virtual scientist" inside your IDE.

## What Was Implemented

### 1. Extended OpenClaw Gating System

**New Fields in `GatingRequirements`:**
- `python_packages`: Vec<String> - Required Python packages (checked via `pip list`)
- `optional_bins`: Vec<String> - Optional binaries (warn if missing, but don't fail)
- `optional_env`: Vec<String> - Optional environment variables
- `optional_config`: Vec<String> - Optional config files

**Implementation Details:**
- Added `python_package_exists()` function that checks installed packages via `python3 -m pip list`
- Enhanced `GatingResult` to include warnings for missing optional requirements
- All gating checks are async-compatible via `tokio::task::spawn_blocking`
- Updated skill registry to log warnings for missing optional requirements

**Tests Added:**
- `test_optional_requirements_warn_but_pass()` - Verifies optional requirements don't fail loading
- `test_python_package_check()` - Tests Python package detection
- `test_mixed_required_and_optional()` - Tests combined required + optional checks
- `test_load_science_skill_with_python_packages()` - Integration test for science skills

### 2. Five Science Capability Skills

All skills are located in `skills/science/` directory:

#### 2.1 Hypothesis & Experiment Designer (`hypothesis-designer.SKILL.md`)
**Purpose:** Turns vague ideas into statistically sound experiments in <60 seconds

**Key Features:**
- Hypothesis generation from raw data or lab notes
- Full experimental design (variables, controls, sample sizes)
- Power analysis (targeting power ≥ 0.8)
- Monte-Carlo pilot simulations
- DOE (Design of Experiments) support

**Requirements:**
```yaml
bins: ["python3"]
python_packages: ["scipy", "numpy", "statsmodels", "pandas"]
optional_bins: ["R"]
optional_env: ["PYTHONPATH"]
```

#### 2.2 Closed-Loop Autonomous Optimizer (`autonomous-optimizer.SKILL.md`)
**Purpose:** Smart Bayesian optimization that runs 24/7 and converges reliably

**Key Features:**
- Bayesian optimization (Gaussian processes, acquisition functions)
- Multi-objective optimization (Pareto front discovery)
- Campaign management with checkpointing
- Live monitoring dashboards
- Convergence diagnostics

**Requirements:**
```yaml
bins: ["python3", "sqlite3"]
python_packages: ["scipy", "numpy", "scikit-learn"]
optional_bins: ["docker"]
optional_python_packages: ["botorch", "optuna", "ax-platform"]
optional_env: ["OPTUNA_STORAGE", "CUDA_VISIBLE_DEVICES"]
```

#### 2.3 Always-On Literature Monitor (`literature-monitor.SKILL.md`)
**Purpose:** Never miss a relevant paper, with automated replication

**Key Features:**
- Background polling of arXiv, bioRxiv, medRxiv, PubMed
- Keyword + embedding-based relevance filtering
- One-click paper replication
- Daily/weekly digests
- Contradiction detection vs your results

**Requirements:**
```yaml
bins: ["python3", "curl", "jq"]
python_packages: ["requests", "feedparser"]
optional_python_packages: ["semanticscholar", "biopython"]
optional_env: ["ARXIV_API_KEY", "PUBMED_API_KEY", "SEMANTIC_SCHOLAR_API_KEY", "OPENAI_API_KEY"]
```

#### 2.4 Scientific Data Engineer (`data-engineer.SKILL.md`)
**Purpose:** Every dataset clean, versioned, and reproducible

**Key Features:**
- Auto-detect and fix units (Pint integration)
- Standardize column naming and handle missing values
- Full provenance tracking (git commit, random seed, environment, hardware)
- Anomaly detection and batch effect correction
- One-click reproducibility packages (Docker + DVC)

**Requirements:**
```yaml
bins: ["python3", "git"]
python_packages: ["pandas", "numpy", "scipy"]
optional_bins: ["dvc", "docker"]
optional_python_packages: ["pint", "great_expectations", "pandera"]
optional_env: ["DVC_REMOTE", "IRONCLAW_REGISTRY_DB"]
```

#### 2.5 Multi-Domain In-Silico Lab (`insilico-lab.SKILL.md`)
**Purpose:** One interface to every simulation engine you own

**Key Features:**
- Plugin system for multiple domains (bio/chem, medical physics, agent-based, CFD, FEM)
- Natural language routing to appropriate backends
- Unified result format with auto-visualization
- Containerized execution (Docker/Singularity)
- Multi-step workflow orchestration

**Requirements:**
```yaml
bins: ["python3"]
python_packages: ["numpy", "scipy", "matplotlib"]
optional_bins: ["docker", "singularity"]
optional_python_packages: ["rdkit", "biopython", "mesa", "pint"]
optional_env: ["SIMULATION_BACKEND_DIR", "PLUGIN_PATH", "MATLAB_PATH"]
```

### 3. Documentation Updates

**FEATURE_PARITY.md:**
- Updated Skills system entry to mention OpenClaw metadata with extended gating
- Added note about 5 bundled science skills in P0 implementation priorities

### 4. Test Coverage

**New Tests:**
- 9 tests in `skills::gating::tests` (all passing)
- 15 tests in `skills::tests` (all passing)
- 23 tests in `skills::registry::tests` (all passing)
- **Total: 82 skills-related tests, all passing**

## File Changes Summary

```
Modified:
  src/skills/mod.rs              - Added new fields to GatingRequirements
  src/skills/gating.rs           - Implemented Python package checking + optional requirements
  src/skills/registry.rs         - Added warning logging for optional requirements + integration test
  FEATURE_PARITY.md              - Updated documentation

Created:
  skills/science/hypothesis-designer.SKILL.md      (116 lines)
  skills/science/autonomous-optimizer.SKILL.md     (169 lines)
  skills/science/literature-monitor.SKILL.md       (194 lines)
  skills/science/data-engineer.SKILL.md            (216 lines)
  skills/science/insilico-lab.SKILL.md            (250 lines)
```

## Usage

### Loading Science Skills

Skills are automatically discovered from `~/.ironclaw/skills/` directory. To use the bundled science skills:

```bash
# Copy bundled skills to user directory
mkdir -p ~/.ironclaw/skills/science
cp skills/science/*.SKILL.md ~/.ironclaw/skills/science/

# Start IronClaw with skills enabled
SKILLS_ENABLED=true ironclaw
```

### Environment Setup for Science Skills

Each skill has specific requirements. Install dependencies as needed:

```bash
# Python scientific stack (common to all skills)
pip install scipy numpy pandas matplotlib

# Hypothesis Designer
pip install statsmodels

# Optimizer
pip install scikit-learn
# Optional: pip install botorch optuna ax-platform

# Literature Monitor
pip install requests feedparser
# Optional: pip install semanticscholar biopython

# Data Engineer
pip install pandas
# Optional: pip install pint great_expectations pandera

# In-Silico Lab
# Optional: pip install rdkit biopython mesa
```

### Gating Behavior

**Required dependencies:**
- If missing, skill will NOT load
- Error logged: "required binary not found: X" or "required Python package not installed: Y"

**Optional dependencies:**
- If missing, skill WILL load
- Warning logged: "optional binary not found: X"
- Skill functionality may be reduced but core features still work

## Architecture Notes

### Why OpenClaw Metadata?

The OpenClaw metadata system provides:
1. **Explicit dependencies** - No silent failures from missing tools
2. **Progressive enhancement** - Optional deps enable extra features
3. **User feedback** - Clear warnings when optional features unavailable
4. **Cross-platform** - Works on Unix (via `which`) and Windows (via `where`)

### Python Package Detection

Uses `python3 -m pip list --format=freeze` to check installed packages:
- Handles both `python3` and `python` commands
- Case-insensitive package name matching
- Matches both `package-name` and `package_name` variations
- Falls back gracefully if Python is not installed

### Trust Model

Bundled skills are marked as `SkillSource::Bundled` and cannot be removed via the API (security boundary). Users must manually delete files or copy to `~/.ironclaw/skills/` to modify.

## Future Enhancements

Potential improvements not included in this implementation:
1. **Auto-discovery of bundled skills** - Load from `skills/` directory at compile time
2. **Binary version checking** - `min_versions: {"python3": "3.9+"}` in requirements
3. **Conda environment support** - Check `conda list` in addition to `pip list`
4. **Container image gating** - `container_image: "ghcr.io/..."` for Docker-based skills
5. **API key validation** - Validate format/structure of API keys in environment

## Validation

All 82 skill-related tests pass:
```bash
$ cargo test --lib skills
running 82 tests
test result: ok. 82 passed; 0 failed; 0 ignored
```

All 5 science skill files validated:
```bash
$ bash validate_science_skills.sh
All 5 science skills are valid!
```

## Commit History

1. `Add Python packages and optional requirements to OpenClaw gating system`
   - Extended GatingRequirements struct
   - Implemented Python package checking
   - Added comprehensive tests

2. `Create 5 science capability SKILL.md files with OpenClaw metadata`
   - Hypothesis & Experiment Designer
   - Closed-Loop Autonomous Optimizer
   - Literature & Preprint Monitor
   - Scientific Data Engineer
   - Multi-Domain In-Silico Lab

3. `Add integration test and update documentation for science skills`
   - Integration test for science skill loading
   - Updated FEATURE_PARITY.md
   - Verified all gating requirements

## References

- **Science IDE Capabilities** (original spec): `/home/runner/work/ironclaw/ironclaw/Science ide capabilities`
- **OpenClaw Reference**: https://github.com/openclaw/openclaw
- **Feature Parity Matrix**: `FEATURE_PARITY.md`

//! Scientific computing tools for agentic science workflows.
//!
//! These tools enable the agent to operate as a 24/7 scientific assistant:
//! - Search scientific literature (PubMed, arXiv, CrossRef)
//! - Perform statistical computations and unit conversions
//! - Track experiments with structured records in workspace memory
//! - Generate structured scientific reports

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;

use crate::context::JobContext;
use crate::tools::tool::{Tool, ToolError, ToolOutput, require_str};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// ScienceSearchTool
// ---------------------------------------------------------------------------

/// Tool for searching scientific literature databases.
///
/// Searches PubMed (NCBI E-utilities), arXiv, and CrossRef — all free,
/// public APIs that require no authentication.
pub struct ScienceSearchTool {
    client: Client,
}

impl ScienceSearchTool {
    /// Create a new science search tool.
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client for ScienceSearchTool");

        Self { client }
    }

    /// Search PubMed via NCBI E-utilities (free, no API key required).
    async fn search_pubmed(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<serde_json::Value, ToolError> {
        // Step 1: esearch to get IDs
        let search_url = format!(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&term={}&retmax={}&retmode=json",
            urlencoding::encode(query),
            max_results
        );
        let search_resp = self
            .client
            .get(&search_url)
            .send()
            .await
            .map_err(|e| ToolError::ExternalService(format!("PubMed search failed: {}", e)))?;
        let search_json: serde_json::Value = search_resp
            .json()
            .await
            .map_err(|e| ToolError::ExternalService(format!("PubMed parse failed: {}", e)))?;

        let ids = search_json["esearchresult"]["idlist"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        if ids.is_empty() {
            return Ok(serde_json::json!({ "source": "pubmed", "results": [], "total": 0 }));
        }

        // Step 2: esummary to get article details
        let id_list = ids.join(",");
        let summary_url = format!(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=pubmed&id={}&retmode=json",
            id_list
        );
        let summary_resp = self
            .client
            .get(&summary_url)
            .send()
            .await
            .map_err(|e| ToolError::ExternalService(format!("PubMed summary failed: {}", e)))?;
        let summary_json: serde_json::Value = summary_resp.json().await.map_err(|e| {
            ToolError::ExternalService(format!("PubMed summary parse failed: {}", e))
        })?;

        let mut articles = Vec::new();
        if let Some(result) = summary_json.get("result") {
            for id in &ids {
                if let Some(article) = result.get(*id) {
                    let authors = article["authors"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|au| au["name"].as_str().map(String::from))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    articles.push(serde_json::json!({
                        "pmid": id,
                        "title": article["title"].as_str().unwrap_or(""),
                        "authors": authors,
                        "journal": article["fulljournalname"].as_str().unwrap_or(""),
                        "pub_date": article["pubdate"].as_str().unwrap_or(""),
                        "doi": article["elocationid"].as_str().unwrap_or(""),
                        "url": format!("https://pubmed.ncbi.nlm.nih.gov/{}/", id),
                    }));
                }
            }
        }

        let total = search_json["esearchresult"]["count"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(serde_json::json!({
            "source": "pubmed",
            "results": articles,
            "total": total,
            "returned": articles.len(),
        }))
    }

    /// Search arXiv via their free Atom API.
    async fn search_arxiv(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<serde_json::Value, ToolError> {
        let url = format!(
            "https://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results={}",
            urlencoding::encode(query),
            max_results
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ToolError::ExternalService(format!("arXiv search failed: {}", e)))?;
        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::ExternalService(format!("arXiv read failed: {}", e)))?;

        // Parse the Atom XML into simple JSON entries
        let articles = parse_arxiv_atom(&body);

        Ok(serde_json::json!({
            "source": "arxiv",
            "results": articles,
            "returned": articles.len(),
        }))
    }

    /// Search CrossRef for DOI-based article metadata (free, no auth).
    async fn search_crossref(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<serde_json::Value, ToolError> {
        let url = format!(
            "https://api.crossref.org/works?query={}&rows={}",
            urlencoding::encode(query),
            max_results
        );
        let resp = self
            .client
            .get(&url)
            .header(
                "User-Agent",
                "IronClaw/0.1 (https://github.com/ncsound919/ironclaw)",
            )
            .send()
            .await
            .map_err(|e| ToolError::ExternalService(format!("CrossRef search failed: {}", e)))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExternalService(format!("CrossRef parse failed: {}", e)))?;

        let items = data["message"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let articles: Vec<serde_json::Value> = items
            .iter()
            .map(|item| {
                let authors = item["author"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|au| {
                                format!(
                                    "{} {}",
                                    au["given"].as_str().unwrap_or(""),
                                    au["family"].as_str().unwrap_or("")
                                )
                                .trim()
                                .to_string()
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let title = item["title"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                serde_json::json!({
                    "doi": item["DOI"].as_str().unwrap_or(""),
                    "title": title,
                    "authors": authors,
                    "journal": item["container-title"].as_array()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    "published": item["published-print"]["date-parts"]
                        .as_array()
                        .or(item["published-online"]["date-parts"].as_array())
                        .and_then(|a| a.first())
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
                    "url": item["URL"].as_str().unwrap_or(""),
                    "type": item["type"].as_str().unwrap_or(""),
                    "citations": item["is-referenced-by-count"].as_u64().unwrap_or(0),
                })
            })
            .collect();

        let total = data["message"]["total-results"].as_u64().unwrap_or(0);

        Ok(serde_json::json!({
            "source": "crossref",
            "results": articles,
            "total": total,
            "returned": articles.len(),
        }))
    }
}

impl Default for ScienceSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ScienceSearchTool {
    fn name(&self) -> &str {
        "science_search"
    }

    fn description(&self) -> &str {
        "Search scientific literature databases (PubMed, arXiv, CrossRef). \
         Use this to find research papers, review articles, and preprints relevant \
         to experiments, assays, or scientific questions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query (e.g., 'CRISPR gene editing efficiency', 'machine learning drug discovery')"
                },
                "source": {
                    "type": "string",
                    "enum": ["pubmed", "arxiv", "crossref", "all"],
                    "description": "Which database to search. 'all' searches all three.",
                    "default": "all"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results per source (default: 5, max: 20)",
                    "default": 5,
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let query = require_str(&params, "query")?;
        let source = params
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        let max_results = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(20) as usize;

        let mut results = serde_json::json!({});

        match source {
            "pubmed" => {
                results["pubmed"] = self.search_pubmed(query, max_results).await?;
            }
            "arxiv" => {
                results["arxiv"] = self.search_arxiv(query, max_results).await?;
            }
            "crossref" => {
                results["crossref"] = self.search_crossref(query, max_results).await?;
            }
            "all" => {
                // Search all sources; collect results from those that succeed
                let mut sources = serde_json::Map::new();
                match self.search_pubmed(query, max_results).await {
                    Ok(v) => {
                        sources.insert("pubmed".to_string(), v);
                    }
                    Err(e) => {
                        sources.insert(
                            "pubmed".to_string(),
                            serde_json::json!({ "error": e.to_string() }),
                        );
                    }
                }
                match self.search_arxiv(query, max_results).await {
                    Ok(v) => {
                        sources.insert("arxiv".to_string(), v);
                    }
                    Err(e) => {
                        sources.insert(
                            "arxiv".to_string(),
                            serde_json::json!({ "error": e.to_string() }),
                        );
                    }
                }
                match self.search_crossref(query, max_results).await {
                    Ok(v) => {
                        sources.insert("crossref".to_string(), v);
                    }
                    Err(e) => {
                        sources.insert(
                            "crossref".to_string(),
                            serde_json::json!({ "error": e.to_string() }),
                        );
                    }
                }
                results = serde_json::Value::Object(sources);
            }
            _ => {
                return Err(ToolError::InvalidParameters(format!(
                    "unknown source: '{}'. Use 'pubmed', 'arxiv', 'crossref', or 'all'",
                    source
                )));
            }
        }

        Ok(ToolOutput::success(
            serde_json::json!({ "query": query, "results": results }),
            start.elapsed(),
        ))
    }

    fn estimated_duration(&self, _params: &serde_json::Value) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn requires_sanitization(&self) -> bool {
        true // External data from scientific APIs
    }

    fn requires_approval(&self) -> bool {
        true // Makes external HTTP requests
    }
}

// ---------------------------------------------------------------------------
// ScienceComputeTool
// ---------------------------------------------------------------------------

/// Tool for scientific computations: statistics, unit conversions, and constants.
pub struct ScienceComputeTool;

#[async_trait]
impl Tool for ScienceComputeTool {
    fn name(&self) -> &str {
        "science_compute"
    }

    fn description(&self) -> &str {
        "Perform scientific computations: descriptive statistics (mean, median, std dev, \
         percentiles), unit conversions (SI, imperial, scientific), and look up physical/chemical \
         constants. Use this for quantitative analysis during experiments and simulations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["statistics", "unit_convert", "constants", "dilution", "molarity"],
                    "description": "The computation to perform"
                },
                "data": {
                    "type": "array",
                    "items": { "type": "number" },
                    "description": "Array of numeric data points (for 'statistics')"
                },
                "value": {
                    "type": "number",
                    "description": "Numeric value to convert (for 'unit_convert', 'dilution', 'molarity')"
                },
                "from_unit": {
                    "type": "string",
                    "description": "Source unit (for 'unit_convert')"
                },
                "to_unit": {
                    "type": "string",
                    "description": "Target unit (for 'unit_convert')"
                },
                "constant": {
                    "type": "string",
                    "description": "Constant name (for 'constants'): avogadro, boltzmann, planck, gas_constant, speed_of_light, faraday, electron_mass, proton_mass, elementary_charge, gravitational"
                },
                "c1": { "type": "number", "description": "Initial concentration (for 'dilution', C1)" },
                "v1": { "type": "number", "description": "Initial volume (for 'dilution', V1)" },
                "c2": { "type": "number", "description": "Final concentration (for 'dilution', C2)" },
                "mass_grams": { "type": "number", "description": "Mass in grams (for 'molarity')" },
                "molecular_weight": { "type": "number", "description": "Molecular weight in g/mol (for 'molarity')" },
                "volume_liters": { "type": "number", "description": "Volume in liters (for 'molarity')" }
            },
            "required": ["operation"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let operation = require_str(&params, "operation")?;

        let result = match operation {
            "statistics" => compute_statistics(&params)?,
            "unit_convert" => compute_unit_conversion(&params)?,
            "constants" => lookup_constant(&params)?,
            "dilution" => compute_dilution(&params)?,
            "molarity" => compute_molarity(&params)?,
            _ => {
                return Err(ToolError::InvalidParameters(format!(
                    "unknown operation: '{}'. Use 'statistics', 'unit_convert', 'constants', 'dilution', or 'molarity'",
                    operation
                )));
            }
        };

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false // Pure computation, no external data
    }
}

// ---------------------------------------------------------------------------
// ExperimentTrackerTool
// ---------------------------------------------------------------------------

/// Tool for tracking experiments in workspace memory.
///
/// Stores experiment records under `experiments/` in the workspace with
/// structured metadata (hypothesis, protocol, observations, results, status).
pub struct ExperimentTrackerTool {
    workspace: Arc<Workspace>,
}

impl ExperimentTrackerTool {
    /// Create a new experiment tracker tool.
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ExperimentTrackerTool {
    fn name(&self) -> &str {
        "experiment_tracker"
    }

    fn description(&self) -> &str {
        "Track scientific experiments in persistent memory. Create experiments with \
         hypotheses and protocols, log observations and measurements, record results, \
         and update experiment status. Data is stored in the workspace under experiments/."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "log_observation", "update_status", "get", "list"],
                    "description": "The action to perform"
                },
                "experiment_id": {
                    "type": "string",
                    "description": "Unique experiment identifier (required for log_observation, update_status, get)"
                },
                "title": {
                    "type": "string",
                    "description": "Experiment title (for 'create')"
                },
                "hypothesis": {
                    "type": "string",
                    "description": "Scientific hypothesis being tested (for 'create')"
                },
                "protocol": {
                    "type": "string",
                    "description": "Experimental protocol/methods description (for 'create')"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tags for categorization (for 'create')"
                },
                "observation": {
                    "type": "string",
                    "description": "Observation or measurement to log (for 'log_observation')"
                },
                "data": {
                    "type": "object",
                    "description": "Structured data associated with the observation (for 'log_observation')"
                },
                "status": {
                    "type": "string",
                    "enum": ["planning", "in_progress", "paused", "completed", "failed", "cancelled"],
                    "description": "Experiment status (for 'update_status')"
                },
                "conclusion": {
                    "type": "string",
                    "description": "Final conclusion (for 'update_status' when status is 'completed' or 'failed')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let action = require_str(&params, "action")?;

        let result = match action {
            "create" => self.create_experiment(&params).await?,
            "log_observation" => self.log_observation(&params).await?,
            "update_status" => self.update_status(&params).await?,
            "get" => self.get_experiment(&params).await?,
            "list" => self.list_experiments().await?,
            _ => {
                return Err(ToolError::InvalidParameters(format!(
                    "unknown action: '{}'. Use 'create', 'log_observation', 'update_status', 'get', or 'list'",
                    action
                )));
            }
        };

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false // Internal workspace data
    }
}

impl ExperimentTrackerTool {
    async fn create_experiment(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let title = require_str(params, "title")?;
        let hypothesis = params
            .get("hypothesis")
            .and_then(|v| v.as_str())
            .unwrap_or("(not specified)");
        let protocol = params
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("(not specified)");
        let tags = params
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let now = chrono::Utc::now();
        let experiment_id = format!(
            "exp-{}-{}",
            now.format("%Y%m%d"),
            &uuid::Uuid::new_v4().to_string()[..8]
        );

        let content = format!(
            "# {}\n\n\
             **ID:** {}\n\
             **Status:** planning\n\
             **Created:** {}\n\
             **Tags:** {}\n\n\
             ## Hypothesis\n\n{}\n\n\
             ## Protocol\n\n{}\n\n\
             ## Observations\n\n\
             _No observations recorded yet._\n\n\
             ## Results\n\n\
             _Experiment not yet completed._\n",
            title,
            experiment_id,
            now.to_rfc3339(),
            if tags.is_empty() {
                "none".to_string()
            } else {
                tags.join(", ")
            },
            hypothesis,
            protocol
        );

        let path = format!("experiments/{}.md", experiment_id);
        self.workspace.write(&path, &content).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to create experiment: {}", e))
        })?;

        Ok(serde_json::json!({
            "status": "created",
            "experiment_id": experiment_id,
            "path": path,
            "title": title,
        }))
    }

    async fn log_observation(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let experiment_id = require_str(params, "experiment_id")?;
        let observation = require_str(params, "observation")?;
        let data = params.get("data");

        let now = chrono::Utc::now();
        let mut entry = format!(
            "\n- **[{}]** {}",
            now.format("%Y-%m-%d %H:%M:%S UTC"),
            observation
        );
        if let Some(data) = data {
            entry.push_str(&format!("\n  - Data: `{}`", data));
        }

        let path = format!("experiments/{}.md", experiment_id);

        // Read existing content to verify experiment exists
        self.workspace.read(&path).await.map_err(|e| {
            ToolError::InvalidParameters(format!("Experiment '{}' not found: {}", experiment_id, e))
        })?;

        self.workspace
            .append(&path, &entry)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to log observation: {}", e)))?;

        Ok(serde_json::json!({
            "status": "logged",
            "experiment_id": experiment_id,
            "timestamp": now.to_rfc3339(),
        }))
    }

    async fn update_status(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let experiment_id = require_str(params, "experiment_id")?;
        let status = require_str(params, "status")?;
        let conclusion = params.get("conclusion").and_then(|v| v.as_str());

        let path = format!("experiments/{}.md", experiment_id);

        // Read existing content
        let doc = self.workspace.read(&path).await.map_err(|e| {
            ToolError::InvalidParameters(format!("Experiment '{}' not found: {}", experiment_id, e))
        })?;

        // Update the status line
        let mut content = doc.content.clone();
        if let Some(pos) = content.find("**Status:**")
            && let Some(end) = content[pos..].find('\n')
        {
            content.replace_range(pos..pos + end, &format!("**Status:** {}", status));
        }

        // Add conclusion to results section if provided
        if let Some(conclusion) = conclusion
            && let Some(pos) = content.find("## Results")
        {
            if let Some(end) = content[pos..].find("\n\n") {
                let insert_pos = pos + end + 2;
                content.insert_str(insert_pos, &format!("{}\n\n", conclusion));
            } else {
                content.push_str(&format!("\n{}\n", conclusion));
            }
        }

        self.workspace
            .write(&path, &content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to update status: {}", e)))?;

        Ok(serde_json::json!({
            "status": "updated",
            "experiment_id": experiment_id,
            "new_status": status,
        }))
    }

    async fn get_experiment(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let experiment_id = require_str(params, "experiment_id")?;
        let path = format!("experiments/{}.md", experiment_id);

        let doc = self.workspace.read(&path).await.map_err(|e| {
            ToolError::InvalidParameters(format!("Experiment '{}' not found: {}", experiment_id, e))
        })?;

        Ok(serde_json::json!({
            "experiment_id": experiment_id,
            "path": path,
            "content": doc.content,
            "updated_at": doc.updated_at.to_rfc3339(),
        }))
    }

    async fn list_experiments(&self) -> Result<serde_json::Value, ToolError> {
        let entries = self.workspace.list("experiments/").await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to list experiments: {}", e))
        })?;

        let experiments: Vec<serde_json::Value> = entries
            .iter()
            .filter(|e| !e.is_directory)
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "name": e.name(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "experiments": experiments,
            "count": experiments.len(),
        }))
    }
}

// ---------------------------------------------------------------------------
// ScienceReportTool
// ---------------------------------------------------------------------------

/// Tool for generating structured scientific reports.
///
/// Produces reports in standard scientific format and stores them in the
/// workspace under `reports/`.
pub struct ScienceReportTool {
    workspace: Arc<Workspace>,
}

impl ScienceReportTool {
    /// Create a new science report tool.
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ScienceReportTool {
    fn name(&self) -> &str {
        "science_report"
    }

    fn description(&self) -> &str {
        "Generate structured scientific reports in standard format (title, abstract, \
         introduction, methods, results, discussion, conclusion, references). \
         Reports are stored in the workspace under reports/."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "get", "list", "append_section"],
                    "description": "Action to perform"
                },
                "report_id": {
                    "type": "string",
                    "description": "Report identifier (for 'get' and 'append_section')"
                },
                "title": {
                    "type": "string",
                    "description": "Report title (for 'create')"
                },
                "abstract": {
                    "type": "string",
                    "description": "Report abstract/summary (for 'create')"
                },
                "introduction": {
                    "type": "string",
                    "description": "Introduction section (for 'create')"
                },
                "methods": {
                    "type": "string",
                    "description": "Methods/materials section (for 'create')"
                },
                "results": {
                    "type": "string",
                    "description": "Results section (for 'create')"
                },
                "discussion": {
                    "type": "string",
                    "description": "Discussion section (for 'create')"
                },
                "conclusion": {
                    "type": "string",
                    "description": "Conclusion section (for 'create')"
                },
                "references": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of references (for 'create')"
                },
                "section_name": {
                    "type": "string",
                    "description": "Section to append to (for 'append_section')"
                },
                "content": {
                    "type": "string",
                    "description": "Content to append (for 'append_section')"
                },
                "experiment_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Linked experiment IDs (for 'create')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();

        let action = require_str(&params, "action")?;

        let result = match action {
            "create" => self.create_report(&params).await?,
            "get" => self.get_report(&params).await?,
            "list" => self.list_reports().await?,
            "append_section" => self.append_section(&params).await?,
            _ => {
                return Err(ToolError::InvalidParameters(format!(
                    "unknown action: '{}'. Use 'create', 'get', 'list', or 'append_section'",
                    action
                )));
            }
        };

        Ok(ToolOutput::success(result, start.elapsed()))
    }

    fn requires_sanitization(&self) -> bool {
        false // Internal workspace data
    }
}

impl ScienceReportTool {
    async fn create_report(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let title = require_str(params, "title")?;
        let abstract_text = params
            .get("abstract")
            .and_then(|v| v.as_str())
            .unwrap_or("_(To be written)_");
        let introduction = params
            .get("introduction")
            .and_then(|v| v.as_str())
            .unwrap_or("_(To be written)_");
        let methods = params
            .get("methods")
            .and_then(|v| v.as_str())
            .unwrap_or("_(To be written)_");
        let results = params
            .get("results")
            .and_then(|v| v.as_str())
            .unwrap_or("_(To be written)_");
        let discussion = params
            .get("discussion")
            .and_then(|v| v.as_str())
            .unwrap_or("_(To be written)_");
        let conclusion = params
            .get("conclusion")
            .and_then(|v| v.as_str())
            .unwrap_or("_(To be written)_");
        let references = params
            .get("references")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let experiment_ids = params
            .get("experiment_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let now = chrono::Utc::now();
        let report_id = format!(
            "rpt-{}-{}",
            now.format("%Y%m%d"),
            &uuid::Uuid::new_v4().to_string()[..8]
        );

        let refs_section = if references.is_empty() {
            "_(No references listed)_".to_string()
        } else {
            references
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {}", i + 1, r))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let linked_experiments = if experiment_ids.is_empty() {
            String::new()
        } else {
            format!("\n**Linked Experiments:** {}\n", experiment_ids.join(", "))
        };

        let content = format!(
            "# {}\n\n\
             **Report ID:** {}\n\
             **Generated:** {}\n\
             {}\n\
             ---\n\n\
             ## Abstract\n\n{}\n\n\
             ## 1. Introduction\n\n{}\n\n\
             ## 2. Methods\n\n{}\n\n\
             ## 3. Results\n\n{}\n\n\
             ## 4. Discussion\n\n{}\n\n\
             ## 5. Conclusion\n\n{}\n\n\
             ## References\n\n{}\n",
            title,
            report_id,
            now.to_rfc3339(),
            linked_experiments,
            abstract_text,
            introduction,
            methods,
            results,
            discussion,
            conclusion,
            refs_section,
        );

        let path = format!("reports/{}.md", report_id);
        self.workspace
            .write(&path, &content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create report: {}", e)))?;

        Ok(serde_json::json!({
            "status": "created",
            "report_id": report_id,
            "path": path,
            "title": title,
        }))
    }

    async fn get_report(&self, params: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let report_id = require_str(params, "report_id")?;
        let path = format!("reports/{}.md", report_id);

        let doc = self.workspace.read(&path).await.map_err(|e| {
            ToolError::InvalidParameters(format!("Report '{}' not found: {}", report_id, e))
        })?;

        Ok(serde_json::json!({
            "report_id": report_id,
            "path": path,
            "content": doc.content,
            "updated_at": doc.updated_at.to_rfc3339(),
        }))
    }

    async fn list_reports(&self) -> Result<serde_json::Value, ToolError> {
        let entries =
            self.workspace.list("reports/").await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to list reports: {}", e))
            })?;

        let reports: Vec<serde_json::Value> = entries
            .iter()
            .filter(|e| !e.is_directory)
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "name": e.name(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "reports": reports,
            "count": reports.len(),
        }))
    }

    async fn append_section(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let report_id = require_str(params, "report_id")?;
        let section_name = require_str(params, "section_name")?;
        let content = require_str(params, "content")?;

        let path = format!("reports/{}.md", report_id);

        // Verify report exists
        self.workspace.read(&path).await.map_err(|e| {
            ToolError::InvalidParameters(format!("Report '{}' not found: {}", report_id, e))
        })?;

        let entry = format!("\n\n### {} (appended)\n\n{}", section_name, content);
        self.workspace
            .append(&path, &entry)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to append section: {}", e)))?;

        Ok(serde_json::json!({
            "status": "appended",
            "report_id": report_id,
            "section": section_name,
        }))
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Parse arXiv Atom XML into simple JSON entries.
///
/// Uses basic string parsing to avoid adding an XML dependency.
fn parse_arxiv_atom(xml: &str) -> Vec<serde_json::Value> {
    let mut articles = Vec::new();

    for entry in xml.split("<entry>").skip(1) {
        let title = extract_xml_tag(entry, "title")
            .map(|t| t.replace('\n', " ").trim().to_string())
            .unwrap_or_default();
        let summary = extract_xml_tag(entry, "summary")
            .map(|s| s.replace('\n', " ").trim().to_string())
            .unwrap_or_default();
        let id = extract_xml_tag(entry, "id").unwrap_or_default();
        let published = extract_xml_tag(entry, "published").unwrap_or_default();

        // Extract authors
        let authors: Vec<String> = entry
            .split("<author>")
            .skip(1)
            .filter_map(|a| extract_xml_tag(a, "name"))
            .collect();

        // Extract categories
        let categories: Vec<String> = entry
            .split("term=\"")
            .skip(1)
            .filter_map(|c| c.split('"').next().map(String::from))
            .collect();

        if !title.is_empty() {
            articles.push(serde_json::json!({
                "title": title,
                "authors": authors,
                "summary": truncate_str(&summary, 500),
                "url": id,
                "published": published,
                "categories": categories,
            }));
        }
    }

    articles
}

/// Extract content between XML tags.
fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)?;
    let content_start = xml[start..].find('>')? + start + 1;
    let end = xml[content_start..].find(&close)? + content_start;
    Some(xml[content_start..end].to_string())
}

/// Truncate a string to a maximum length, adding "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let boundary = s
            .char_indices()
            .take_while(|(i, _)| *i < max_len.saturating_sub(3))
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!("{}...", &s[..boundary])
    }
}

/// Compute descriptive statistics.
fn compute_statistics(params: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let data = params
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            ToolError::InvalidParameters("'data' array required for statistics".to_string())
        })?;

    let values: Vec<f64> = data.iter().filter_map(|v| v.as_f64()).collect();

    if values.is_empty() {
        return Err(ToolError::InvalidParameters(
            "'data' must contain at least one number".to_string(),
        ));
    }

    let n = values.len() as f64;
    let sum: f64 = values.iter().sum();
    let mean = sum / n;

    let mut sorted = values.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let median = if sorted.len().is_multiple_of(2) {
        let mid = sorted.len() / 2;
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };

    let variance = values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();

    // Sample standard deviation (Bessel's correction)
    let sample_variance = if values.len() > 1 {
        values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)
    } else {
        0.0
    };
    let sample_std_dev = sample_variance.sqrt();

    let min = sorted.first().copied().unwrap_or(0.0);
    let max = sorted.last().copied().unwrap_or(0.0);

    let percentile = |p: f64| -> f64 {
        let rank = p / 100.0 * (sorted.len() as f64 - 1.0);
        let lower = rank.floor() as usize;
        let upper = rank.ceil() as usize;
        if lower == upper {
            sorted[lower]
        } else {
            sorted[lower] * (upper as f64 - rank) + sorted[upper] * (rank - lower as f64)
        }
    };

    // Standard error of the mean
    let sem = sample_std_dev / n.sqrt();

    Ok(serde_json::json!({
        "n": values.len(),
        "mean": mean,
        "median": median,
        "std_dev": std_dev,
        "sample_std_dev": sample_std_dev,
        "sem": sem,
        "variance": variance,
        "sample_variance": sample_variance,
        "min": min,
        "max": max,
        "range": max - min,
        "sum": sum,
        "percentiles": {
            "p25": percentile(25.0),
            "p50": percentile(50.0),
            "p75": percentile(75.0),
            "p90": percentile(90.0),
            "p95": percentile(95.0),
            "p99": percentile(99.0),
        },
        "iqr": percentile(75.0) - percentile(25.0),
    }))
}

/// Perform unit conversions.
fn compute_unit_conversion(params: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let value = params
        .get("value")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            ToolError::InvalidParameters("'value' number required for unit_convert".to_string())
        })?;
    let from = require_str(params, "from_unit")?;
    let to = require_str(params, "to_unit")?;

    let result = convert_units(value, from, to)?;

    Ok(serde_json::json!({
        "input": value,
        "from_unit": from,
        "to_unit": to,
        "result": result,
    }))
}

/// Convert between units. Supports common scientific units.
fn convert_units(value: f64, from: &str, to: &str) -> Result<f64, ToolError> {
    // Normalize unit names to lowercase
    let from = from.to_lowercase();
    let to = to.to_lowercase();

    // Convert to a base unit first, then to the target unit
    let (base_value, base_unit) = to_base_unit(value, &from)?;
    from_base_unit(base_value, &base_unit, &to)
}

/// Convert a value to its base SI unit.
fn to_base_unit(value: f64, unit: &str) -> Result<(f64, String), ToolError> {
    match unit {
        // Length -> meters
        "m" | "meter" | "meters" => Ok((value, "m".to_string())),
        "km" | "kilometer" | "kilometers" => Ok((value * 1000.0, "m".to_string())),
        "cm" | "centimeter" | "centimeters" => Ok((value * 0.01, "m".to_string())),
        "mm" | "millimeter" | "millimeters" => Ok((value * 0.001, "m".to_string())),
        "um" | "micrometer" | "micrometers" | "micron" | "microns" => {
            Ok((value * 1e-6, "m".to_string()))
        }
        "nm" | "nanometer" | "nanometers" => Ok((value * 1e-9, "m".to_string())),
        "pm" | "picometer" | "picometers" => Ok((value * 1e-12, "m".to_string())),
        "angstrom" | "angstroms" | "å" => Ok((value * 1e-10, "m".to_string())),
        "in" | "inch" | "inches" => Ok((value * 0.0254, "m".to_string())),
        "ft" | "foot" | "feet" => Ok((value * 0.3048, "m".to_string())),
        "mi" | "mile" | "miles" => Ok((value * 1609.344, "m".to_string())),

        // Mass -> kilograms
        "kg" | "kilogram" | "kilograms" => Ok((value, "kg".to_string())),
        "g" | "gram" | "grams" => Ok((value * 0.001, "kg".to_string())),
        "mg" | "milligram" | "milligrams" => Ok((value * 1e-6, "kg".to_string())),
        "ug" | "microgram" | "micrograms" => Ok((value * 1e-9, "kg".to_string())),
        "ng" | "nanogram" | "nanograms" => Ok((value * 1e-12, "kg".to_string())),
        "lb" | "pound" | "pounds" => Ok((value * 0.453592, "kg".to_string())),
        "oz" | "ounce" | "ounces" => Ok((value * 0.0283495, "kg".to_string())),
        "dalton" | "daltons" | "da" | "amu" => Ok((value * 1.66053906660e-27, "kg".to_string())),

        // Volume -> liters
        "l" | "liter" | "liters" | "litre" | "litres" => Ok((value, "l".to_string())),
        "ml" | "milliliter" | "milliliters" => Ok((value * 0.001, "l".to_string())),
        "ul" | "microliter" | "microliters" => Ok((value * 1e-6, "l".to_string())),
        "nl" | "nanoliter" | "nanoliters" => Ok((value * 1e-9, "l".to_string())),
        "gal" | "gallon" | "gallons" => Ok((value * 3.78541, "l".to_string())),

        // Temperature -> kelvin
        "k" | "kelvin" => Ok((value, "k".to_string())),
        "c" | "celsius" => Ok((value + 273.15, "k".to_string())),
        "f" | "fahrenheit" => Ok(((value - 32.0) * 5.0 / 9.0 + 273.15, "k".to_string())),

        // Time -> seconds
        "s" | "sec" | "second" | "seconds" => Ok((value, "s".to_string())),
        "ms" | "millisecond" | "milliseconds" => Ok((value * 0.001, "s".to_string())),
        "us" | "microsecond" | "microseconds" => Ok((value * 1e-6, "s".to_string())),
        "ns" | "nanosecond" | "nanoseconds" => Ok((value * 1e-9, "s".to_string())),
        "min" | "minute" | "minutes" => Ok((value * 60.0, "s".to_string())),
        "h" | "hr" | "hour" | "hours" => Ok((value * 3600.0, "s".to_string())),
        "day" | "days" => Ok((value * 86400.0, "s".to_string())),

        // Pressure -> pascals
        "pa" | "pascal" | "pascals" => Ok((value, "pa".to_string())),
        "kpa" | "kilopascal" | "kilopascals" => Ok((value * 1000.0, "pa".to_string())),
        "bar" => Ok((value * 100000.0, "pa".to_string())),
        "atm" | "atmosphere" | "atmospheres" => Ok((value * 101325.0, "pa".to_string())),
        "mmhg" | "torr" => Ok((value * 133.322, "pa".to_string())),
        "psi" => Ok((value * 6894.76, "pa".to_string())),

        // Concentration -> mol/L (molar)
        "mol/l" | "molar" | "mol/liter" => Ok((value, "mol/l".to_string())),
        "mmol/l" | "millimolar" => Ok((value * 0.001, "mol/l".to_string())),
        "umol/l" | "micromolar" => Ok((value * 1e-6, "mol/l".to_string())),
        "nmol/l" | "nanomolar" => Ok((value * 1e-9, "mol/l".to_string())),

        // Energy -> joules
        "j" | "joule" | "joules" => Ok((value, "j".to_string())),
        "kj" | "kilojoule" | "kilojoules" => Ok((value * 1000.0, "j".to_string())),
        "cal" | "calorie" | "calories" => Ok((value * 4.184, "j".to_string())),
        "kcal" | "kilocalorie" | "kilocalories" => Ok((value * 4184.0, "j".to_string())),
        "ev" | "electronvolt" | "electronvolts" => Ok((value * 1.602176634e-19, "j".to_string())),

        _ => Err(ToolError::InvalidParameters(format!(
            "unknown unit: '{}'. Supported: length (m, km, cm, mm, um, nm, pm, angstrom, in, ft, mi), \
             mass (kg, g, mg, ug, ng, lb, oz, dalton), \
             volume (l, ml, ul, nl, gal), \
             temperature (k, c, f), \
             time (s, ms, us, ns, min, h, day), \
             pressure (pa, kpa, bar, atm, mmhg, psi), \
             concentration (mol/l, mmol/l, umol/l, nmol/l), \
             energy (j, kj, cal, kcal, ev)",
            unit
        ))),
    }
}

/// Convert from a base unit to the target unit.
fn from_base_unit(value: f64, base: &str, target: &str) -> Result<f64, ToolError> {
    // Convert base unit to target unit (inverse of to_base_unit)
    match (base, target) {
        // Length (base: meters)
        ("m", "m" | "meter" | "meters") => Ok(value),
        ("m", "km" | "kilometer" | "kilometers") => Ok(value / 1000.0),
        ("m", "cm" | "centimeter" | "centimeters") => Ok(value / 0.01),
        ("m", "mm" | "millimeter" | "millimeters") => Ok(value / 0.001),
        ("m", "um" | "micrometer" | "micrometers" | "micron" | "microns") => Ok(value / 1e-6),
        ("m", "nm" | "nanometer" | "nanometers") => Ok(value / 1e-9),
        ("m", "pm" | "picometer" | "picometers") => Ok(value / 1e-12),
        ("m", "angstrom" | "angstroms" | "å") => Ok(value / 1e-10),
        ("m", "in" | "inch" | "inches") => Ok(value / 0.0254),
        ("m", "ft" | "foot" | "feet") => Ok(value / 0.3048),
        ("m", "mi" | "mile" | "miles") => Ok(value / 1609.344),

        // Mass (base: kg)
        ("kg", "kg" | "kilogram" | "kilograms") => Ok(value),
        ("kg", "g" | "gram" | "grams") => Ok(value / 0.001),
        ("kg", "mg" | "milligram" | "milligrams") => Ok(value / 1e-6),
        ("kg", "ug" | "microgram" | "micrograms") => Ok(value / 1e-9),
        ("kg", "ng" | "nanogram" | "nanograms") => Ok(value / 1e-12),
        ("kg", "lb" | "pound" | "pounds") => Ok(value / 0.453592),
        ("kg", "oz" | "ounce" | "ounces") => Ok(value / 0.0283495),
        ("kg", "dalton" | "daltons" | "da" | "amu") => Ok(value / 1.66053906660e-27),

        // Volume (base: liters)
        ("l", "l" | "liter" | "liters" | "litre" | "litres") => Ok(value),
        ("l", "ml" | "milliliter" | "milliliters") => Ok(value / 0.001),
        ("l", "ul" | "microliter" | "microliters") => Ok(value / 1e-6),
        ("l", "nl" | "nanoliter" | "nanoliters") => Ok(value / 1e-9),
        ("l", "gal" | "gallon" | "gallons") => Ok(value / 3.78541),

        // Temperature (base: kelvin)
        ("k", "k" | "kelvin") => Ok(value),
        ("k", "c" | "celsius") => Ok(value - 273.15),
        ("k", "f" | "fahrenheit") => Ok((value - 273.15) * 9.0 / 5.0 + 32.0),

        // Time (base: seconds)
        ("s", "s" | "sec" | "second" | "seconds") => Ok(value),
        ("s", "ms" | "millisecond" | "milliseconds") => Ok(value / 0.001),
        ("s", "us" | "microsecond" | "microseconds") => Ok(value / 1e-6),
        ("s", "ns" | "nanosecond" | "nanoseconds") => Ok(value / 1e-9),
        ("s", "min" | "minute" | "minutes") => Ok(value / 60.0),
        ("s", "h" | "hr" | "hour" | "hours") => Ok(value / 3600.0),
        ("s", "day" | "days") => Ok(value / 86400.0),

        // Pressure (base: pascals)
        ("pa", "pa" | "pascal" | "pascals") => Ok(value),
        ("pa", "kpa" | "kilopascal" | "kilopascals") => Ok(value / 1000.0),
        ("pa", "bar") => Ok(value / 100000.0),
        ("pa", "atm" | "atmosphere" | "atmospheres") => Ok(value / 101325.0),
        ("pa", "mmhg" | "torr") => Ok(value / 133.322),
        ("pa", "psi") => Ok(value / 6894.76),

        // Concentration (base: mol/L)
        ("mol/l", "mol/l" | "molar" | "mol/liter") => Ok(value),
        ("mol/l", "mmol/l" | "millimolar") => Ok(value / 0.001),
        ("mol/l", "umol/l" | "micromolar") => Ok(value / 1e-6),
        ("mol/l", "nmol/l" | "nanomolar") => Ok(value / 1e-9),

        // Energy (base: joules)
        ("j", "j" | "joule" | "joules") => Ok(value),
        ("j", "kj" | "kilojoule" | "kilojoules") => Ok(value / 1000.0),
        ("j", "cal" | "calorie" | "calories") => Ok(value / 4.184),
        ("j", "kcal" | "kilocalorie" | "kilocalories") => Ok(value / 4184.0),
        ("j", "ev" | "electronvolt" | "electronvolts") => Ok(value / 1.602176634e-19),

        _ => Err(ToolError::InvalidParameters(format!(
            "cannot convert from '{}' base to '{}'. Units must be in the same category.",
            base, target
        ))),
    }
}

/// Look up a physical/chemical constant.
fn lookup_constant(params: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let name = require_str(params, "constant")?;

    let (value, unit, description) = match name.to_lowercase().as_str() {
        "avogadro" | "na" => (6.02214076e23, "mol⁻¹", "Avogadro's number"),
        "boltzmann" | "kb" => (1.380649e-23, "J/K", "Boltzmann constant"),
        "planck" | "h" => (6.62607015e-34, "J·s", "Planck constant"),
        "hbar" | "reduced_planck" => (1.054571817e-34, "J·s", "Reduced Planck constant (ℏ)"),
        "gas_constant" | "r" => (8.314462618, "J/(mol·K)", "Universal gas constant"),
        "speed_of_light" | "c" => (2.99792458e8, "m/s", "Speed of light in vacuum"),
        "faraday" | "f" => (96485.33212, "C/mol", "Faraday constant"),
        "electron_mass" | "me" => (9.1093837015e-31, "kg", "Electron mass"),
        "proton_mass" | "mp" => (1.67262192369e-27, "kg", "Proton mass"),
        "neutron_mass" | "mn" => (1.67492749804e-27, "kg", "Neutron mass"),
        "elementary_charge" | "e" => (1.602176634e-19, "C", "Elementary charge"),
        "gravitational" | "g" => (6.67430e-11, "m³/(kg·s²)", "Gravitational constant"),
        "standard_gravity" | "g0" => (9.80665, "m/s²", "Standard acceleration of gravity"),
        "vacuum_permittivity" | "epsilon0" => (8.8541878128e-12, "F/m", "Vacuum permittivity (ε₀)"),
        "vacuum_permeability" | "mu0" => (1.25663706212e-6, "H/m", "Vacuum permeability (μ₀)"),
        "stefan_boltzmann" | "sigma" => (5.670374419e-8, "W/(m²·K⁴)", "Stefan–Boltzmann constant"),
        "water_molar_mass" => (18.01528, "g/mol", "Molar mass of water"),
        _ => {
            return Err(ToolError::InvalidParameters(format!(
                "unknown constant: '{}'. Available: avogadro, boltzmann, planck, hbar, \
                 gas_constant, speed_of_light, faraday, electron_mass, proton_mass, \
                 neutron_mass, elementary_charge, gravitational, standard_gravity, \
                 vacuum_permittivity, vacuum_permeability, stefan_boltzmann, water_molar_mass",
                name
            )));
        }
    };

    Ok(serde_json::json!({
        "name": description,
        "symbol": name,
        "value": value,
        "unit": unit,
    }))
}

/// Compute dilution using C1*V1 = C2*V2.
fn compute_dilution(params: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let c1 = params.get("c1").and_then(|v| v.as_f64());
    let v1 = params.get("v1").and_then(|v| v.as_f64());
    let c2 = params.get("c2").and_then(|v| v.as_f64());
    let v2 = params.get("value").and_then(|v| v.as_f64()); // V2 passed as 'value'

    // Solve for the missing variable
    match (c1, v1, c2, v2) {
        (Some(c1), Some(v1), Some(c2), None) => {
            if c2 <= 0.0 {
                return Err(ToolError::InvalidParameters(
                    "C2 must be > 0 to solve for V2".to_string(),
                ));
            }
            let v2 = (c1 * v1) / c2;
            Ok(serde_json::json!({
                "c1": c1, "v1": v1, "c2": c2, "v2": v2,
                "formula": "C1×V1 = C2×V2",
                "solved_for": "V2",
            }))
        }
        (Some(c1), Some(v1), None, Some(v2)) => {
            if v2 <= 0.0 {
                return Err(ToolError::InvalidParameters(
                    "V2 must be > 0 to solve for C2".to_string(),
                ));
            }
            let c2 = (c1 * v1) / v2;
            Ok(serde_json::json!({
                "c1": c1, "v1": v1, "c2": c2, "v2": v2,
                "formula": "C1×V1 = C2×V2",
                "solved_for": "C2",
            }))
        }
        (Some(c1), None, Some(c2), Some(v2)) => {
            if c1 <= 0.0 {
                return Err(ToolError::InvalidParameters(
                    "C1 must be > 0 to solve for V1".to_string(),
                ));
            }
            let v1 = (c2 * v2) / c1;
            Ok(serde_json::json!({
                "c1": c1, "v1": v1, "c2": c2, "v2": v2,
                "formula": "C1×V1 = C2×V2",
                "solved_for": "V1",
            }))
        }
        (None, Some(v1), Some(c2), Some(v2)) => {
            if v1 <= 0.0 {
                return Err(ToolError::InvalidParameters(
                    "V1 must be > 0 to solve for C1".to_string(),
                ));
            }
            let c1 = (c2 * v2) / v1;
            Ok(serde_json::json!({
                "c1": c1, "v1": v1, "c2": c2, "v2": v2,
                "formula": "C1×V1 = C2×V2",
                "solved_for": "C1",
            }))
        }
        _ => Err(ToolError::InvalidParameters(
            "provide exactly 3 of: c1, v1, c2, value (as V2). The fourth will be solved."
                .to_string(),
        )),
    }
}

/// Compute molarity: M = (mass / molecular_weight) / volume.
fn compute_molarity(params: &serde_json::Value) -> Result<serde_json::Value, ToolError> {
    let mass = params
        .get("mass_grams")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            ToolError::InvalidParameters("'mass_grams' required for molarity".to_string())
        })?;
    let mw = params
        .get("molecular_weight")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            ToolError::InvalidParameters("'molecular_weight' required for molarity".to_string())
        })?;
    let vol = params
        .get("volume_liters")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            ToolError::InvalidParameters("'volume_liters' required for molarity".to_string())
        })?;

    if mw <= 0.0 {
        return Err(ToolError::InvalidParameters(
            "molecular_weight must be > 0".to_string(),
        ));
    }
    if vol <= 0.0 {
        return Err(ToolError::InvalidParameters(
            "volume_liters must be > 0".to_string(),
        ));
    }

    let moles = mass / mw;
    let molarity = moles / vol;

    Ok(serde_json::json!({
        "mass_grams": mass,
        "molecular_weight": mw,
        "volume_liters": vol,
        "moles": moles,
        "molarity_mol_per_l": molarity,
        "molarity_mmol_per_l": molarity * 1000.0,
        "formula": "M = (mass / MW) / volume",
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ScienceSearchTool tests --

    #[test]
    fn test_science_search_schema() {
        let tool = ScienceSearchTool::new();
        assert_eq!(tool.name(), "science_search");
        assert!(tool.requires_sanitization());
        assert!(tool.requires_approval());

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["source"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&"query".into())
        );
    }

    #[test]
    fn test_science_search_invalid_source() {
        let tool = ScienceSearchTool::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ctx = JobContext::default();
        let result = rt.block_on(tool.execute(
            serde_json::json!({"query": "test", "source": "invalid"}),
            &ctx,
        ));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown source"));
    }

    // -- ScienceComputeTool tests --

    #[test]
    fn test_science_compute_schema() {
        let tool = ScienceComputeTool;
        assert_eq!(tool.name(), "science_compute");
        assert!(!tool.requires_sanitization());
        assert!(!tool.requires_approval());

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["operation"].is_object());
    }

    #[tokio::test]
    async fn test_statistics_basic() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "statistics",
                    "data": [1.0, 2.0, 3.0, 4.0, 5.0]
                }),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert_eq!(r["n"], 5);
        assert!((r["mean"].as_f64().unwrap() - 3.0).abs() < 1e-10);
        assert!((r["median"].as_f64().unwrap() - 3.0).abs() < 1e-10);
        assert!((r["min"].as_f64().unwrap() - 1.0).abs() < 1e-10);
        assert!((r["max"].as_f64().unwrap() - 5.0).abs() < 1e-10);
        assert!((r["sum"].as_f64().unwrap() - 15.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_statistics_empty_data() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"operation": "statistics", "data": []}),
                &ctx,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_unit_conversion_temperature() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "unit_convert",
                    "value": 100.0,
                    "from_unit": "c",
                    "to_unit": "f"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert!((r["result"].as_f64().unwrap() - 212.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_unit_conversion_length() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "unit_convert",
                    "value": 1.0,
                    "from_unit": "km",
                    "to_unit": "m"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert!((r["result"].as_f64().unwrap() - 1000.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_unit_conversion_mass() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "unit_convert",
                    "value": 1.0,
                    "from_unit": "kg",
                    "to_unit": "g"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert!((r["result"].as_f64().unwrap() - 1000.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_constants_lookup() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"operation": "constants", "constant": "avogadro"}),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert!((r["value"].as_f64().unwrap() - 6.02214076e23).abs() < 1e16);
        assert_eq!(r["unit"], "mol⁻¹");
    }

    #[tokio::test]
    async fn test_constants_unknown() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(
                serde_json::json!({"operation": "constants", "constant": "unknown"}),
                &ctx,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dilution_solve_v2() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        // C1=10, V1=5, C2=2 → V2 = (10*5)/2 = 25
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "dilution",
                    "c1": 10.0, "v1": 5.0, "c2": 2.0
                }),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert!((r["v2"].as_f64().unwrap() - 25.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_molarity() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        // 58.44g NaCl (MW=58.44) in 1L → 1 mol/L
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "molarity",
                    "mass_grams": 58.44,
                    "molecular_weight": 58.44,
                    "volume_liters": 1.0
                }),
                &ctx,
            )
            .await
            .unwrap();

        let r = &result.result;
        assert!((r["molarity_mol_per_l"].as_f64().unwrap() - 1.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn test_unknown_operation() {
        let tool = ScienceComputeTool;
        let ctx = JobContext::default();
        let result = tool
            .execute(serde_json::json!({"operation": "invalid"}), &ctx)
            .await;
        assert!(result.is_err());
    }

    // -- arXiv XML parsing tests --

    #[test]
    fn test_parse_arxiv_atom() {
        let xml = r#"<feed>
        <entry>
            <title>Test Paper Title</title>
            <summary>This is a test summary.</summary>
            <id>http://arxiv.org/abs/2401.00001v1</id>
            <published>2024-01-01T00:00:00Z</published>
            <author><name>Alice Smith</name></author>
            <author><name>Bob Jones</name></author>
            <category term="cs.AI"/>
        </entry>
        </feed>"#;

        let articles = parse_arxiv_atom(xml);
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0]["title"], "Test Paper Title");
        assert_eq!(articles[0]["authors"][0], "Alice Smith");
        assert_eq!(articles[0]["authors"][1], "Bob Jones");
    }

    #[test]
    fn test_parse_arxiv_atom_empty() {
        let articles = parse_arxiv_atom("<feed></feed>");
        assert!(articles.is_empty());
    }

    // -- Helper function tests --

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
    }

    #[test]
    fn test_extract_xml_tag() {
        assert_eq!(
            extract_xml_tag("<title>Hello</title>", "title"),
            Some("Hello".to_string())
        );
        assert_eq!(extract_xml_tag("<root>no match</root>", "title"), None);
    }

    #[test]
    fn test_convert_incompatible_units() {
        let result = convert_units(1.0, "kg", "c");
        assert!(result.is_err());
    }
}

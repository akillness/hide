//! `jeopi -p`, one child per request, and `jeopi models --json` for its model catalog.

use serde_json::Value;

use crate::program::Program;
use crate::runner::{self, Environment, Spec};
use crate::text_cli::{self, TextCliConfig, classify_failure, json_from_text, system_with_schema};
use crate::{
    AiBackend, AiError, AiRequest, AiResponse, AiUsage, Availability, CancelToken, ModelCatalog,
    ProviderId,
};

const PROGRAM: &str = "jeopi";
const INSTRUCTION: &str = "Follow the system instructions for the input above.";

pub struct JeopiCliBackend {
    config: TextCliConfig,
}

impl JeopiCliBackend {
    pub fn new(config: TextCliConfig) -> Self {
        Self { config }
    }

    pub fn print_arguments(
        model: &str,
        effort: Option<&str>,
        system: &str,
        output_schema: &Value,
    ) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--no-tools",
            "--no-lsp",
            "--no-session",
            "--no-extensions",
            "--no-skills",
            "--no-rules",
            "--thinking",
            effort.unwrap_or("off"),
            "--system-prompt",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(system_with_schema(system, output_schema));
        if !model.is_empty() {
            args.push("--model".to_owned());
            args.push(model.to_owned());
        }
        args.push(INSTRUCTION.to_owned());
        args
    }

    fn list_models(&self, binary: &Program) -> Result<Vec<String>, String> {
        let run = text_cli::probe(
            binary,
            &["models", "--json"],
            &self.config.cwd,
            "jeopi_models",
        )?;
        if !run.succeeded() {
            return Err(format!("jeopi_models_failed:exit={}", run.exit()));
        }
        Ok(parse_models(&run.stdout))
    }
}

fn parse_models(stdout: &str) -> Vec<String> {
    let mut models = Vec::new();
    if let Ok(parsed) = serde_json::from_str::<Value>(stdout.trim())
        && let Some(items) = parsed.get("models").and_then(Value::as_array)
    {
        for item in items {
            let selector = item
                .get("selector")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str));
            if let Some(selector) = selector
                && !selector.is_empty()
                && !models.iter().any(|existing| existing == selector)
            {
                models.push(selector.to_owned());
            }
        }
        if !models.is_empty() {
            return models;
        }
    }
    for line in stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let reference = tokens
            .iter()
            .find(|token| token.contains('/') && !token.starts_with('/') && !token.ends_with('/'))
            .map(|token| (*token).to_owned());
        if let Some(reference) = reference
            && !models.contains(&reference)
        {
            models.push(reference);
        }
    }
    models
}

impl AiBackend for JeopiCliBackend {
    fn id(&self) -> ProviderId {
        ProviderId::JEOPI
    }

    fn availability(&self) -> Availability {
        let Some(binary) = self.config.resolved(PROGRAM) else {
            return Availability::NotInstalled;
        };
        match self.list_models(&binary) {
            Ok(models) if !models.is_empty() => Availability::Ready,
            Ok(_) => Availability::NeedsLogin,
            Err(reason) => Availability::Unavailable { reason },
        }
    }

    fn models(&self) -> ModelCatalog {
        let Some(binary) = self.config.resolved(PROGRAM) else {
            return ModelCatalog::Unknown {
                reason: "jeopi_not_installed".to_owned(),
            };
        };
        match self.list_models(&binary) {
            Ok(models) if !models.is_empty() => ModelCatalog::Offered(models),
            Ok(_) => ModelCatalog::Unknown {
                reason: "jeopi_models_empty".to_owned(),
            },
            Err(reason) => ModelCatalog::Unknown { reason },
        }
    }

    fn execute(&self, request: &AiRequest, cancel: &CancelToken) -> Result<AiResponse, AiError> {
        let binary = self
            .config
            .resolved(PROGRAM)
            .ok_or_else(|| AiError::ProviderUnavailable("jeopi_not_installed".to_owned()))?;
        let run = runner::run(
            &Spec {
                binary: &binary,
                args: &Self::print_arguments(
                    request.model(&self.config.model),
                    request.effort(ProviderId::JEOPI),
                    &request.system_prompt(),
                    &request.output_schema,
                ),
                cwd: &self.config.cwd,
                stdin: Some(&request.input),
                environment: Environment::Inherit,
                set: &[],
                deadline: request.deadline,
            },
            cancel,
        )
        .map_err(|error| error.into_error("jeopi"))?;
        if !run.succeeded() {
            return Err(classify_failure("jeopi", run.code, &run.stderr));
        }
        let value = json_from_text(&run.stdout)
            .ok_or_else(|| AiError::InvalidOutput("jeopi_answer_not_json".to_owned()))?;
        Ok(AiResponse {
            value,
            usage: AiUsage::default(),
        })
    }
}

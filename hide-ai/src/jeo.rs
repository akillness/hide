//! `jeo` in headless one-shot mode, one child per request, reading `~/.jeo` for models and auth state.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::runner::{self, Environment, Spec};
use crate::text_cli::{TextCliConfig, classify_failure, json_from_text, system_with_schema};
use crate::{
    AiBackend, AiError, AiRequest, AiResponse, AiUsage, Availability, CancelToken, ModelCatalog,
    ProviderId,
};

const PROGRAM: &str = "jeo";
const INSTRUCTION: &str = "Follow the system instructions for the input above.";

pub struct JeoCliBackend {
    config: TextCliConfig,
    home: Option<PathBuf>,
}

impl JeoCliBackend {
    pub fn new(config: TextCliConfig) -> Self {
        Self {
            config,
            home: hide_platform::host::home_dir().ok(),
        }
    }

    pub fn print_arguments(
        model: &str,
        effort: Option<&str>,
        system: &str,
        output_schema: &Value,
    ) -> Vec<String> {
        let mut args: Vec<String> = [
            "--no-tui",
            "--no-tools",
            "--no-session",
            "--no-skills",
            "--system-prompt",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(system_with_schema(system, output_schema));
        if !model.is_empty() {
            args.push("--model".to_owned());
            args.push(model.to_owned());
        }
        if let Some(effort) = effort {
            args.push("--thinking".to_owned());
            args.push(effort.to_owned());
        }
        args
    }

    fn read_catalog(&self) -> (bool, Vec<String>) {
        let Some(home) = &self.home else {
            return (false, Vec::new());
        };
        read_jeo_home(home)
    }
}

fn read_jeo_home(home: &Path) -> (bool, Vec<String>) {
    let jeo_dir = home.join(".jeo");
    let mut models = Vec::new();
    let mut signed_in = false;
    let push_unique = |models: &mut Vec<String>, item: &str| {
        if !item.is_empty() && !models.iter().any(|existing| existing == item) {
            models.push(item.to_owned());
        }
    };

    if let Ok(bytes) = std::fs::read(jeo_dir.join("config.json"))
        && let Ok(config) = serde_json::from_slice::<Value>(&bytes)
    {
        if config
            .get("oauth")
            .and_then(Value::as_object)
            .is_some_and(|map| !map.is_empty())
            || config
                .get("providers")
                .and_then(Value::as_object)
                .is_some_and(|map| !map.is_empty())
        {
            signed_in = true;
        }
        if let Some(default_model) = config.get("defaultModel").and_then(Value::as_str) {
            push_unique(&mut models, default_model);
        }
        if let Some(recent) = config.get("recentModels").and_then(Value::as_array) {
            for entry in recent {
                if let Some(model) = entry.as_str() {
                    push_unique(&mut models, model);
                }
            }
        }
    }

    if let Ok(bytes) = std::fs::read(jeo_dir.join("model-catalog-cache.json"))
        && let Ok(cache) = serde_json::from_slice::<Value>(&bytes)
        && let Some(providers) = cache.get("providers").and_then(Value::as_array)
    {
        for provider in providers {
            if let Some(list) = provider.get("models").and_then(Value::as_array) {
                for entry in list {
                    if let Some(model) = entry.as_str() {
                        push_unique(&mut models, model);
                        signed_in = true;
                    }
                }
            }
        }
    }

    (signed_in, models)
}

impl AiBackend for JeoCliBackend {
    fn id(&self) -> ProviderId {
        ProviderId::JEO
    }

    fn availability(&self) -> Availability {
        if self.config.resolved(PROGRAM).is_none() {
            return Availability::NotInstalled;
        };
        let (signed_in, models) = self.read_catalog();
        if signed_in || !models.is_empty() {
            Availability::Ready
        } else {
            Availability::NeedsLogin
        }
    }

    fn models(&self) -> ModelCatalog {
        if self.config.resolved(PROGRAM).is_none() {
            return ModelCatalog::Unknown {
                reason: "jeo_not_installed".to_owned(),
            };
        };
        let (_, models) = self.read_catalog();
        if !models.is_empty() {
            ModelCatalog::Offered(models)
        } else {
            ModelCatalog::Unknown {
                reason: "jeo_models_empty".to_owned(),
            }
        }
    }

    fn execute(&self, request: &AiRequest, cancel: &CancelToken) -> Result<AiResponse, AiError> {
        let binary = self
            .config
            .resolved(PROGRAM)
            .ok_or_else(|| AiError::ProviderUnavailable("jeo_not_installed".to_owned()))?;
        let stdin_payload = format!("{}\n\n{INSTRUCTION}", request.input);
        let run = runner::run(
            &Spec {
                binary: &binary,
                args: &Self::print_arguments(
                    request.model(&self.config.model),
                    request.effort(ProviderId::JEO),
                    &request.system_prompt(),
                    &request.output_schema,
                ),
                cwd: &self.config.cwd,
                stdin: Some(&stdin_payload),
                environment: Environment::Inherit,
                set: &[],
                deadline: request.deadline,
            },
            cancel,
        )
        .map_err(|error| error.into_error("jeo"))?;
        if !run.succeeded() {
            return Err(classify_failure("jeo", run.code, &run.stderr));
        }
        let value = json_from_text(&run.stdout)
            .ok_or_else(|| AiError::InvalidOutput("jeo_answer_not_json".to_owned()))?;
        Ok(AiResponse {
            value,
            usage: AiUsage::default(),
        })
    }
}

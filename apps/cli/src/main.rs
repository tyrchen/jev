//! Command-line interface for structured Jev decisions through `OpenRouter`.

#![forbid(unsafe_code)]
#![warn(rust_2024_compatibility, missing_docs, missing_debug_implementations)]

mod args;
mod input;
mod output;
mod transport;

use std::{
    env,
    io::{self, Write},
    process::ExitCode,
    time::Duration,
};

use args::{Cli, Command, NoulArgs};
use clap::{Parser, error::ErrorKind};
use jev_core::{DecisionQuestion, ModelId, NoulCriteria, QuestionId, State, StateFormat};
use reqwest::Client;
use rustls::crypto::{CryptoProvider, aws_lc_rs::default_provider};
use serde_json::{Map, Value};
use transport::TransportError;

const MAX_API_KEY_BYTES: usize = 4_096;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                ExitCode::SUCCESS
            } else {
                let message = sanitize_diagnostic(&error.to_string());
                let _ = writeln!(io::stderr().lock(), "jev: {message}");
                ExitCode::from(2)
            };
            return code;
        }
    };

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = sanitize_diagnostic(&error.to_string());
            let _ = writeln!(io::stderr().lock(), "jev: {message}");
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let mut stdout = io::stdout();
    run_with_transport(cli, None, &OpenRouterTransport, &mut stdout).await
}

trait DecisionTransport {
    async fn send(
        &self,
        client: &Client,
        api_key: &str,
        request: &Value,
        question_id: &QuestionId,
        question: &DecisionQuestion,
    ) -> Result<Value, TransportError>;
}

struct OpenRouterTransport;

impl DecisionTransport for OpenRouterTransport {
    async fn send(
        &self,
        client: &Client,
        api_key: &str,
        request: &Value,
        question_id: &QuestionId,
        question: &DecisionQuestion,
    ) -> Result<Value, TransportError> {
        transport::send(client, api_key, request, question_id, question).await
    }
}

async fn run_with_transport<T: DecisionTransport, W: Write>(
    cli: Cli,
    api_key_override: Option<&str>,
    decisions_transport: &T,
    stdout: &mut W,
) -> Result<(), CliError> {
    let source = input::InputSource::resolve(&cli).map_err(CliError::usage)?;
    let question = build_question(cli.command).map_err(CliError::usage)?;
    let question_id = cli.id;
    let model = cli.model;

    let api_key = if cli.dry_run {
        None
    } else {
        Some(match api_key_override {
            Some(api_key) => api_key.to_owned(),
            None => read_api_key().map_err(CliError::usage)?,
        })
    };
    let client = if api_key.is_some() {
        install_tls_provider().map_err(CliError::runtime)?;
        Some(build_http_client(
            Duration::from_secs(cli.connect_timeout),
            Duration::from_secs(cli.request_timeout),
        )?)
    } else {
        None
    };

    let input = source
        .acquire(Duration::from_secs(cli.input_timeout))
        .await
        .map_err(CliError::usage)?;
    let state = State::parse(&input, StateFormat::from(cli.state_format))
        .map_err(|error| CliError::usage(error.to_string()))?;
    let request = build_request(&model, &state, &question_id, &question)
        .map_err(|_| CliError::runtime("could not serialize the validated request"))?;

    let response = if cli.dry_run {
        request
    } else {
        let api_key = api_key
            .as_deref()
            .ok_or_else(|| CliError::runtime("OPENROUTER_API_KEY is unavailable".to_owned()))?;
        let client = client
            .as_ref()
            .ok_or_else(|| CliError::runtime("HTTP client is unavailable".to_owned()))?;
        decisions_transport
            .send(client, api_key, &request, &question_id, &question)
            .await
            .map_err(|error| CliError::runtime(error.to_string()))?
    };

    let selected = if cli.dry_run {
        &response
    } else {
        output::value_for_output(&response, cli.answers_only)
            .map_err(|message| CliError::runtime(message.to_owned()))?
    };
    match output::write_json(stdout, selected, cli.output) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(CliError::output(error.to_string())),
    }
}

fn build_question(command: Command) -> Result<DecisionQuestion, String> {
    match command {
        Command::Noul(args) => build_noul(args),
        Command::Choice(args) => DecisionQuestion::choice(args.instructions, args.options)
            .map_err(|error| error.to_string()),
        Command::Score(args) => DecisionQuestion::score(args.instructions, args.levels)
            .map_err(|error| error.to_string()),
    }
}

fn build_noul(args: NoulArgs) -> Result<DecisionQuestion, String> {
    let criteria = match (args.true_criteria, args.false_criteria) {
        (Some(true_description), Some(false_description)) => Some(
            NoulCriteria::new(true_description, false_description)
                .map_err(|error| error.to_string())?,
        ),
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => {
            return Err("provide both --true-criteria and --false-criteria, or neither".to_owned());
        }
    };
    DecisionQuestion::noul(args.instructions, criteria).map_err(|error| error.to_string())
}

fn build_request(
    model: &ModelId,
    state: &State,
    question_id: &QuestionId,
    question: &DecisionQuestion,
) -> Result<Value, serde_json::Error> {
    let mut questions = Map::new();
    questions.insert(question_id.as_str().to_owned(), question.to_value()?);
    let mut request = Map::new();
    request.insert("model".to_owned(), Value::String(model.as_str().to_owned()));
    request.insert("state".to_owned(), state.as_value().clone());
    request.insert("questions".to_owned(), Value::Object(questions));
    Ok(Value::Object(request))
}

fn read_api_key() -> Result<String, String> {
    let api_key = env::var("OPENROUTER_API_KEY")
        .map_err(|_| "OPENROUTER_API_KEY is not set; export an OpenRouter API key".to_owned())?;
    if api_key.is_empty()
        || api_key.len() > MAX_API_KEY_BYTES
        || !api_key.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(
            "OPENROUTER_API_KEY must be printable ASCII and no longer than 4096 bytes".to_owned(),
        );
    }
    Ok(api_key)
}

fn install_tls_provider() -> Result<(), String> {
    if CryptoProvider::get_default().is_some() {
        return Ok(());
    }
    default_provider()
        .install_default()
        .map_err(|_| "could not select the AWS-LC Rustls provider".to_owned())
}

fn build_http_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<Client, CliError> {
    Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build()
        .map_err(|_| CliError::runtime("could not initialize the HTTPS client".to_owned()))
}

fn sanitize_diagnostic(input: &str) -> String {
    let mut output = String::with_capacity(input.len().min(8_192));
    for character in input.chars() {
        if output.len() >= 8_192 {
            output.push_str("...");
            break;
        }
        if character.is_control() {
            output.extend(character.escape_default());
        } else {
            output.push(character);
        }
    }
    output
}

#[derive(Debug)]
struct CliError {
    code: u8,
    message: String,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    fn output(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    fn exit_code(&self) -> u8 {
        self.code
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use clap::Parser;
    use jev_core::{QuestionId, State, StateFormat};
    use serde_json::json;

    use super::{Cli, build_question, build_request};

    #[test]
    fn test_should_build_exact_request_envelope_with_custom_model_and_id() {
        let cli = Cli::try_parse_from([
            "jev",
            "--state",
            "A payment failed",
            "--model",
            "typesafe/jev-1.13",
            "--id",
            "ticket_check",
            "noul",
            "Is this a payment bug?",
        ])
        .unwrap();
        let question = build_question(cli.command).unwrap();
        let state = State::parse("A payment failed", StateFormat::Text).unwrap();
        let request = build_request(&cli.model, &state, &cli.id, &question).unwrap();
        assert_eq!(
            request,
            json!({
                "model":"typesafe/jev-1.13",
                "state":"A payment failed",
                "questions":{"ticket_check":{"type":"noul","instructions":"Is this a payment bug?"}}
            })
        );
        assert_eq!(
            QuestionId::from_str("ticket_check").unwrap().as_str(),
            "ticket_check"
        );
    }

    #[test]
    fn test_should_reject_unpaired_criteria_before_input_or_credentials() {
        assert!(
            Cli::try_parse_from([
                "jev",
                "noul",
                "Is this safe?",
                "--true-criteria",
                "Read-only",
            ])
            .is_err()
        );
    }

    #[test]
    fn test_should_reject_conflicting_state_sources_and_bad_model_syntax() {
        assert!(
            Cli::try_parse_from([
                "jev",
                "--state",
                "inline",
                "--state-file",
                "state.json",
                "noul",
                "Is this safe?",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["jev", "--model", "bad\nmodel", "noul", "Question",]).is_err()
        );
    }
}

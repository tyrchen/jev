use std::str::FromStr;

use jev_core::{
    DecisionAnswer, DecisionQuestion, InputError, ModelId, QuestionId, parse_bounded_json_value,
};
use reqwest::{Client, StatusCode};
use serde_json::{Map, Value};
use thiserror::Error;

const DECISIONS_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_RESPONSE_FIELD_BYTES: usize = 256;
const MAX_REQUEST_ID_BYTES: usize = 128;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error(
        "request timed out; increase --request-timeout if this call is expected to take longer"
    )]
    Timeout,
    #[error("could not connect to OpenRouter")]
    Network,
    #[error("OpenRouter returned HTTP {status}: {guidance}{request_id}")]
    HttpStatus {
        status: u16,
        guidance: &'static str,
        request_id: String,
    },
    #[error(
        "OpenRouter HTTP {status} response exceeded the 1048576-byte limit: {guidance}{request_id}"
    )]
    ResponseTooLarge {
        status: u16,
        guidance: &'static str,
        request_id: String,
    },
    #[error("OpenRouter response was not valid UTF-8 JSON")]
    InvalidJson,
    #[error("OpenRouter response exceeded a protocol resource limit or had duplicate keys")]
    InvalidResponseJson,
    #[error("OpenRouter response did not match the Decisions API protocol: {reason}")]
    Protocol { reason: &'static str },
}

pub async fn send(
    client: &Client,
    api_key: &str,
    request: &Value,
    question_id: &QuestionId,
    question: &DecisionQuestion,
) -> Result<Value, TransportError> {
    send_to(
        client,
        DECISIONS_ENDPOINT,
        api_key,
        request,
        question_id,
        question,
    )
    .await
}

async fn send_to(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    request: &Value,
    question_id: &QuestionId,
    question: &DecisionQuestion,
) -> Result<Value, TransportError> {
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .header("X-OpenRouter-Title", "jev")
        .header("X-Title", "jev")
        .header("X-OpenRouter-Categories", "cli-agent")
        .json(request)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                TransportError::Timeout
            } else {
                TransportError::Network
            }
        })?;

    let status = response.status();
    let request_id = response_request_id(response.headers());
    let body = read_response_body(response, status, request_id.clone()).await?;
    if !status.is_success() {
        return Err(TransportError::HttpStatus {
            status: status.as_u16(),
            guidance: status_guidance(status),
            request_id,
        });
    }
    let body = std::str::from_utf8(&body).map_err(|_| TransportError::InvalidJson)?;
    let response =
        parse_bounded_json_value(body).map_err(|error| map_response_json_error(&error))?;
    validate_response(&response, question_id, question)?;
    Ok(response)
}

async fn read_response_body(
    mut response: reqwest::Response,
    status: StatusCode,
    request_id: String,
) -> Result<Vec<u8>, TransportError> {
    let oversized = || TransportError::ResponseTooLarge {
        status: status.as_u16(),
        guidance: if status.is_success() {
            "the response body exceeded the local size policy"
        } else {
            status_guidance(status)
        },
        request_id: request_id.clone(),
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(oversized());
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            TransportError::Timeout
        } else {
            TransportError::Network
        }
    })? {
        let next_length = body.len().checked_add(chunk.len()).ok_or_else(&oversized)?;
        if next_length > MAX_RESPONSE_BYTES {
            return Err(oversized());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_response_json_error(error: &InputError) -> TransportError {
    match error {
        InputError::DuplicateJsonKey
        | InputError::StateJsonLimit { .. }
        | InputError::StateTooLarge { .. } => TransportError::InvalidResponseJson,
        _ => TransportError::InvalidJson,
    }
}

fn response_request_id(headers: &reqwest::header::HeaderMap) -> String {
    ["x-openrouter-request-id", "x-request-id"]
        .iter()
        .find_map(|name| headers.get(*name).and_then(safe_request_id))
        .map_or_else(String::new, |id| format!(" (request ID {id})"))
}

fn safe_request_id(value: &reqwest::header::HeaderValue) -> Option<String> {
    let value = value.to_str().ok()?;
    if value.is_empty()
        || value.len() > MAX_REQUEST_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(value.chars().flat_map(char::escape_default).collect())
}

fn status_guidance(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED => "authentication failed; check OPENROUTER_API_KEY",
        StatusCode::PAYMENT_REQUIRED => "the account needs additional credits",
        StatusCode::FORBIDDEN => "the API key is not permitted to make this request",
        StatusCode::TOO_MANY_REQUESTS => "rate limited; wait before sending another request",
        value if value.is_server_error() => "OpenRouter or its provider reported a server error",
        _ => "the request was rejected; check the model and question configuration",
    }
}

fn validate_response(
    response: &Value,
    expected_id: &QuestionId,
    question: &DecisionQuestion,
) -> Result<(), TransportError> {
    let object = response.as_object().ok_or(TransportError::Protocol {
        reason: "envelope is not an object",
    })?;
    bounded_text(object, "model", true)?;
    bounded_optional_text(object, "id")?;
    bounded_optional_text(object, "provider")?;

    let answers =
        object
            .get("answers")
            .and_then(Value::as_object)
            .ok_or(TransportError::Protocol {
                reason: "answers is missing or is not an object",
            })?;
    if answers.len() != 1 {
        return Err(TransportError::Protocol {
            reason: "expected exactly one answer for this command",
        });
    }
    let answer = answers
        .get(expected_id.as_str())
        .ok_or(TransportError::Protocol {
            reason: "the requested answer ID is missing or unexpected IDs were returned",
        })?;
    DecisionAnswer::parse(question, answer).map_err(|_| TransportError::Protocol {
        reason: "answer fields do not match the requested primitive or criteria",
    })?;

    let usage = object
        .get("usage")
        .and_then(Value::as_object)
        .ok_or(TransportError::Protocol {
            reason: "usage is missing or is not an object",
        })?;
    for field in ["input_tokens", "output_tokens"] {
        if usage.get(field).and_then(Value::as_u64).is_none() {
            return Err(TransportError::Protocol {
                reason: "usage token counts are missing or invalid",
            });
        }
    }
    if let Some(cost) = usage.get("cost").filter(|value| !value.is_null())
        && cost
            .as_f64()
            .is_none_or(|cost| !cost.is_finite() || cost < 0.0)
    {
        return Err(TransportError::Protocol {
            reason: "usage cost is invalid",
        });
    }
    validate_collection_limits(response)?;
    Ok(())
}

fn bounded_text(
    object: &Map<String, Value>,
    field: &'static str,
    required: bool,
) -> Result<(), TransportError> {
    let Some(value) = object.get(field) else {
        return if required {
            Err(TransportError::Protocol {
                reason: "required envelope fields are missing",
            })
        } else {
            Ok(())
        };
    };
    let text = value.as_str().ok_or(TransportError::Protocol {
        reason: "envelope text field has the wrong type",
    })?;
    if text.is_empty()
        || text.len() > MAX_RESPONSE_FIELD_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(TransportError::Protocol {
            reason: "envelope text field is empty, too long, or contains controls",
        });
    }
    if field == "model" && ModelId::from_str(text).is_err() {
        return Err(TransportError::Protocol {
            reason: "returned model ID has invalid syntax",
        });
    }
    Ok(())
}

fn bounded_optional_text(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), TransportError> {
    if object.get(field).is_some_and(|value| !value.is_null()) {
        bounded_text(object, field, false)?;
    }
    Ok(())
}

fn validate_collection_limits(value: &Value) -> Result<(), TransportError> {
    let mut pending = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((current, depth)) = pending.pop() {
        if depth > 64 {
            return Err(TransportError::Protocol {
                reason: "response nesting limit was exceeded",
            });
        }
        nodes += 1;
        if nodes > 100_000 {
            return Err(TransportError::Protocol {
                reason: "response node limit was exceeded",
            });
        }
        match current {
            Value::Array(values) => {
                if values.len() > MAX_RESPONSE_BYTES {
                    return Err(TransportError::Protocol {
                        reason: "response collection limit was exceeded",
                    });
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() > MAX_RESPONSE_BYTES {
                    return Err(TransportError::Protocol {
                        reason: "response collection limit was exceeded",
                    });
                }
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
fn status_guidance_for_test(status: u16) -> &'static str {
    StatusCode::from_u16(status).map_or("the request failed", status_guidance)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        sync::mpsc,
        thread,
        time::Duration,
    };

    use clap::Parser;
    use rustls::crypto::aws_lc_rs::default_provider;
    use serde_json::json;

    use super::*;
    use crate::{Cli, DecisionTransport, run_with_transport};

    struct FixtureTransport(String);

    impl DecisionTransport for FixtureTransport {
        async fn send(
            &self,
            client: &Client,
            api_key: &str,
            request: &Value,
            question_id: &QuestionId,
            question: &DecisionQuestion,
        ) -> Result<Value, TransportError> {
            send_to(client, &self.0, api_key, request, question_id, question).await
        }
    }

    fn noul_question() -> DecisionQuestion {
        DecisionQuestion::noul("Is this a bug?", None).unwrap()
    }

    fn question_id() -> QuestionId {
        QuestionId::from_str("ticket_check").unwrap()
    }

    fn request_value() -> Value {
        json!({
            "model":"typesafe/jev-1.13",
            "state":{"ticket":"It fails"},
            "questions":{"ticket_check":{"type":"noul","instructions":"Is this a bug?"}}
        })
    }

    fn valid_response() -> Value {
        json!({
            "id":"gen-dec-123",
            "model":"typesafe/jev-1.13-20260917",
            "provider":"TypeSafe",
            "answers":{"ticket_check":{"type":"noul","noul":0.96,"future_answer_metadata":{"trace":"ok"}}},
            "usage":{"input_tokens":476,"output_tokens":70,"cost":0.00002},
            "future_envelope_field":{"kept":true}
        })
    }

    fn response_with_answer(answer: &Value) -> Value {
        json!({
            "model":"typesafe/jev-1.13-20260917",
            "answers":{"ticket_check":answer},
            "usage":{"input_tokens":1,"output_tokens":1}
        })
    }

    fn client() -> Client {
        client_with_timeout(Duration::from_secs(2))
    }

    fn client_with_timeout(timeout: Duration) -> Client {
        let _provider_installed = default_provider().install_default();
        Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .unwrap()
    }

    fn serve_once(response: Vec<u8>) -> (String, mpsc::Receiver<Vec<u8>>, thread::JoinHandle<()>) {
        serve_once_with_delay(response, Duration::ZERO)
    }

    fn serve_once_with_delay(
        response: Vec<u8>,
        delay: Duration,
    ) -> (String, mpsc::Receiver<Vec<u8>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!(
            "http://{}/api/alpha/decisions",
            listener.local_addr().unwrap()
        );
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            read_request_and_reply(stream, &response, &sender, delay);
        });
        (address, receiver, handle)
    }

    fn read_request_and_reply(
        mut stream: TcpStream,
        response: &[u8],
        sender: &mpsc::Sender<Vec<u8>>,
        delay: Duration,
    ) {
        let clone = stream.try_clone().unwrap();
        let mut reader = BufReader::new(clone);
        let mut request = Vec::new();
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        request.extend_from_slice(request_line.as_bytes());
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                request.extend_from_slice(line.as_bytes());
                break;
            }
            request.extend_from_slice(line.as_bytes());
            headers.push(line);
        }
        let length = headers
            .iter()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        request.extend_from_slice(&body);
        let _ = sender.send(request);
        thread::sleep(delay);
        let _ = stream.write_all(response);
    }

    fn response_bytes(status: &str, body: &[u8], headers: &str) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    #[tokio::test]
    async fn test_should_send_exact_decisions_request_and_keep_additive_metadata() {
        let body = serde_json::to_vec(&valid_response()).unwrap();
        let (endpoint, captured_request, server) = serve_once(response_bytes("200 OK", &body, ""));
        let request = request_value();
        let response = send_to(
            &client(),
            &endpoint,
            "test-key",
            &request,
            &question_id(),
            &noul_question(),
        )
        .await
        .unwrap();
        server.join().unwrap();
        let sent = captured_request.recv().unwrap();
        assert!(sent.starts_with(b"POST /api/alpha/decisions HTTP/1.1\r\n"));
        assert!(
            String::from_utf8_lossy(&sent)
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        let body_start = sent
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let request: Value = serde_json::from_slice(&sent[body_start..]).unwrap();
        assert_eq!(request, request_value());
        assert_eq!(response["future_envelope_field"], json!({"kept":true}));
        assert_eq!(
            response["answers"]["ticket_check"]["future_answer_metadata"]["trace"],
            "ok"
        );
    }

    #[tokio::test]
    async fn test_should_run_complete_cli_success_and_failure_against_fixtures() {
        let body = serde_json::to_vec(&valid_response()).unwrap();
        let (endpoint, captured_request, server) = serve_once(response_bytes("200 OK", &body, ""));
        let transport = FixtureTransport(endpoint);
        let cli = Cli::try_parse_from([
            "jev",
            "--state",
            "The save button fails",
            "--id",
            "ticket_check",
            "--answers-only",
            "--output",
            "pretty",
            "noul",
            "Is this a bug?",
        ])
        .unwrap();
        let mut output = Vec::new();
        run_with_transport(cli, Some("test-key"), &transport, &mut output)
            .await
            .unwrap();
        server.join().unwrap();
        let sent = captured_request.recv().unwrap();
        let body_start = sent
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let request: Value = serde_json::from_slice(&sent[body_start..]).unwrap();
        assert_eq!(request["state"], "The save button fails");
        assert_eq!(request["questions"]["ticket_check"]["type"], "noul");
        assert!(output.starts_with(b"{\n"));
        assert_eq!(output.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<Value>(&output[..output.len() - 1]).unwrap(),
            json!({"ticket_check":{"type":"noul","noul":0.96,"future_answer_metadata":{"trace":"ok"}}})
        );

        let mut invalid_response = valid_response();
        invalid_response["answers"] = json!({});
        let body = serde_json::to_vec(&invalid_response).unwrap();
        let (endpoint, _, server) = serve_once(response_bytes("200 OK", &body, ""));
        let transport = FixtureTransport(endpoint);
        let cli = Cli::try_parse_from([
            "jev",
            "--state",
            "The save button fails",
            "--id",
            "ticket_check",
            "noul",
            "Is this a bug?",
        ])
        .unwrap();
        let mut output = Vec::new();
        let error = run_with_transport(cli, Some("test-key"), &transport, &mut output)
            .await
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.exit_code(), 1);
        assert!(error.to_string().contains("Decisions API protocol"));
        assert!(output.is_empty());
    }

    #[test]
    fn test_should_reject_missing_extra_and_malformed_answers() {
        let question = noul_question();
        let id = question_id();
        let mut response = valid_response();
        assert!(validate_response(&response, &id, &question).is_ok());
        response["answers"] = json!({});
        assert!(validate_response(&response, &id, &question).is_err());
        response["answers"] = json!({"ticket_check":{"type":"noul","noul":2.0}});
        assert!(validate_response(&response, &id, &question).is_err());
        response["answers"] = json!({
            "ticket_check":{"type":"noul","noul":0.2},
            "other":{"type":"noul","noul":0.2}
        });
        assert!(validate_response(&response, &id, &question).is_err());
    }

    #[test]
    fn test_should_validate_choice_and_score_response_fixtures() {
        let id = question_id();
        let choice = DecisionQuestion::choice(
            "Which team?",
            vec![
                jev_core::ChoiceOption::new("billing", "Payments").unwrap(),
                jev_core::ChoiceOption::new("technical", "Product").unwrap(),
            ],
        )
        .unwrap();
        let choice_response = response_with_answer(&json!({
            "type":"choice",
            "choice":"billing",
            "confidence":0.75,
            "probabilities":{"billing":0.84,"technical":0.16}
        }));
        assert!(validate_response(&choice_response, &id, &choice).is_ok());

        let score = DecisionQuestion::score(
            "How urgent?",
            vec!["Low".into(), "Medium".into(), "High".into()],
        )
        .unwrap();
        let score_response = response_with_answer(&json!({
            "type":"score",
            "score":1.99,
            "confidence":0.99,
            "probabilities":{"0":0.0,"1":0.01,"2":0.99},
            "legend":{"0":"Low","1":"Medium","2":"High"}
        }));
        assert!(validate_response(&score_response, &id, &score).is_ok());
    }

    #[test]
    fn test_should_reject_duplicate_keys_and_hide_malformed_response_bodies() {
        let duplicate = parse_bounded_json_value(r#"{"model":"one","model":"two"}"#).unwrap_err();
        assert_eq!(
            map_response_json_error(&duplicate),
            TransportError::InvalidResponseJson
        );
    }

    #[tokio::test]
    async fn test_should_hide_malformed_response_body_from_diagnostics() {
        let body = b"{\"secret\":\"private state\\u001b[31m\"";
        let (endpoint, _, server) = serve_once(response_bytes("200 OK", body, ""));
        let error = send_to(
            &client(),
            &endpoint,
            "test-key",
            &request_value(),
            &question_id(),
            &noul_question(),
        )
        .await
        .unwrap_err();
        server.join().unwrap();
        let rendered = error.to_string();
        assert!(!rendered.contains("private state"));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[tokio::test]
    async fn test_should_return_safe_actionable_http_errors_without_body_text() {
        for (status, guidance) in [
            (401, "authentication failed"),
            (402, "additional credits"),
            (429, "rate limited"),
            (500, "server error"),
        ] {
            let body = b"{\"error\":\"SENSITIVE BODY\\u001b[31m\"}";
            let (endpoint, _, server) = serve_once(response_bytes(
                &format!("{status} Error"),
                body,
                "X-OpenRouter-Request-Id: req-abc\r\n",
            ));
            let error = send_to(
                &client(),
                &endpoint,
                "test-key",
                &request_value(),
                &question_id(),
                &noul_question(),
            )
            .await
            .unwrap_err();
            server.join().unwrap();
            let rendered = error.to_string();
            assert!(rendered.contains(guidance));
            assert!(rendered.contains("req-abc"));
            assert!(!rendered.contains("SENSITIVE"));
            assert!(!rendered.contains('\u{1b}'));
        }
    }

    #[tokio::test]
    async fn test_should_reject_content_length_response_over_cap_before_reading() {
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        )
        .into_bytes();
        let (endpoint, _, server) = serve_once(raw);
        let result = send_to(
            &client(),
            &endpoint,
            "test-key",
            &request_value(),
            &question_id(),
            &noul_question(),
        )
        .await;
        server.join().unwrap();
        assert!(matches!(
            result,
            Err(TransportError::ResponseTooLarge { status: 200, .. })
        ));
    }

    #[tokio::test]
    async fn test_should_reject_chunked_response_over_cap_while_reading() {
        let mut raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        let first = vec![b'x'; MAX_RESPONSE_BYTES];
        raw.extend_from_slice(format!("{:x}\r\n", first.len()).as_bytes());
        raw.extend_from_slice(&first);
        raw.extend_from_slice(b"\r\n1\r\nx\r\n0\r\n\r\n");
        let (endpoint, _, server) = serve_once(raw);
        let result = send_to(
            &client(),
            &endpoint,
            "test-key",
            &request_value(),
            &question_id(),
            &noul_question(),
        )
        .await;
        server.join().unwrap();
        assert!(matches!(
            result,
            Err(TransportError::ResponseTooLarge { status: 200, .. })
        ));
    }

    #[tokio::test]
    async fn test_should_bound_chunked_error_bodies_without_exposing_them() {
        let mut raw = b"HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\nX-Request-Id: req-auth\r\nConnection: close\r\n\r\n".to_vec();
        let first = vec![b's'; MAX_RESPONSE_BYTES];
        raw.extend_from_slice(format!("{:x}\r\n", first.len()).as_bytes());
        raw.extend_from_slice(&first);
        raw.extend_from_slice(b"\r\n1\r\nx\r\n0\r\n\r\n");
        let (endpoint, _, server) = serve_once(raw);
        let result = send_to(
            &client(),
            &endpoint,
            "test-key",
            &request_value(),
            &question_id(),
            &noul_question(),
        )
        .await;
        server.join().unwrap();
        match result {
            Err(TransportError::ResponseTooLarge {
                status,
                guidance,
                request_id,
            }) => {
                assert_eq!(status, 401);
                assert!(guidance.contains("authentication"));
                assert!(request_id.contains("req-auth"));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_should_apply_request_deadline_without_retrying() {
        let body = serde_json::to_vec(&valid_response()).unwrap();
        let (endpoint, captured_request, server) = serve_once_with_delay(
            response_bytes("200 OK", &body, ""),
            Duration::from_millis(250),
        );
        let result = send_to(
            &client_with_timeout(Duration::from_millis(30)),
            &endpoint,
            "test-key",
            &request_value(),
            &question_id(),
            &noul_question(),
        )
        .await;
        assert_eq!(result, Err(TransportError::Timeout));
        server.join().unwrap();
        let sent = captured_request.recv().unwrap();
        assert!(sent.starts_with(b"POST /api/alpha/decisions HTTP/1.1\r\n"));
    }

    #[test]
    fn test_should_bound_external_request_ids_and_keep_status_guidance() {
        assert!(safe_request_id(&"a".repeat(MAX_REQUEST_ID_BYTES).parse().unwrap()).is_some());
        assert!(safe_request_id(&"a".repeat(MAX_REQUEST_ID_BYTES + 1).parse().unwrap()).is_none());
        assert!(status_guidance_for_test(401).contains("authentication"));
    }
}

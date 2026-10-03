use std::io::{self, Write};

use serde_json::Value;

use crate::args::OutputMode;

pub fn value_for_output(value: &Value, answers_only: bool) -> Result<&Value, &'static str> {
    if !answers_only {
        return Ok(value);
    }
    value
        .get("answers")
        .filter(|answers| answers.is_object())
        .ok_or("validated response has no answers object")
}

pub fn encode_json(value: &Value, mode: OutputMode) -> Result<Vec<u8>, serde_json::Error> {
    match mode {
        OutputMode::Json => serde_json::to_vec(value),
        OutputMode::Pretty => serde_json::to_vec_pretty(value),
    }
}

pub fn write_json<W: Write>(writer: &mut W, value: &Value, mode: OutputMode) -> io::Result<()> {
    let encoded = encode_json(value, mode).map_err(io::Error::other)?;
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use serde_json::json;

    use super::{encode_json, value_for_output, write_json};
    use crate::args::OutputMode;

    #[test]
    fn test_should_encode_compact_and_pretty_json_with_newline() {
        for mode in [OutputMode::Json, OutputMode::Pretty] {
            let value = json!({"answers":{"answer":{"type":"noul","noul":0.7}}});
            let mut output = Vec::new();
            write_json(&mut output, &value, mode).unwrap();
            assert_eq!(output.last(), Some(&b'\n'));
            let parsed: serde_json::Value =
                serde_json::from_slice(&output[..output.len() - 1]).unwrap();
            assert_eq!(parsed, value);
        }
    }

    #[test]
    fn test_should_select_answers_only_as_a_json_value() {
        let response = json!({"model":"snapshot","answers":{"custom":{"type":"noul","noul":0.7}}});
        let answers = value_for_output(&response, true).unwrap();
        assert_eq!(answers, &json!({"custom":{"type":"noul","noul":0.7}}));
        assert!(value_for_output(&json!({}), true).is_err());
    }

    struct BrokenPipe;

    impl Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_should_preserve_broken_pipe_error_for_the_process_policy() {
        let error = write_json(&mut BrokenPipe, &json!({"ok":true}), OutputMode::Json)
            .expect_err("writer should report its broken pipe");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn test_should_emit_valid_json_in_each_answers_only_output_format() {
        let response = json!({"model":"snapshot","answers":{"answer":{"type":"noul","noul":0.7}}});
        for mode in [OutputMode::Json, OutputMode::Pretty] {
            let selected = value_for_output(&response, true).unwrap();
            let bytes = encode_json(selected, mode).unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(parsed, json!({"answer":{"type":"noul","noul":0.7}}));
        }
    }
}

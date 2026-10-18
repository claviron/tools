pub type CommandResult = Result<(), Box<dyn std::error::Error>>;

/// What `main` returns, so that a failure reaches the operator as the sentence
/// the server wrote.
///
/// Rust prints an error returned from `main` with `Debug`. For a refusal this
/// tool produced itself that is the quoted message, but a refusal that came
/// over gRPC arrives as a `tonic::Status`, whose `Debug` is the entire
/// envelope — code, metadata headers, date, `source: None` — with the sentence
/// buried inside it. Preserve that sentence when present; when a server or
/// transport returns an empty status message, retain the gRPC code and source
/// so the CLI never reports a blank error. See `claviron/SEMANTICS.md` S26.
pub struct CliError(Box<dyn std::error::Error>);

impl std::fmt::Debug for CliError {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        match self.0.downcast_ref::<tonic::Status>() {
            Some(status) if !status.message().trim().is_empty() => {
                f.write_str(status.message())
            }
            Some(status) => match std::error::Error::source(status) {
                Some(source) => write!(f, "gRPC {:?}: {source}", status.code()),
                None => write!(f, "gRPC request failed ({:?})", status.code()),
            },
            None => std::fmt::Display::fmt(&self.0, f),
        }
    }
}

impl<E: Into<Box<dyn std::error::Error>>> From<E> for CliError {
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

pub fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn print_operation(
    success: bool,
    message: Option<String>,
    failure: &str,
) {
    if success {
        println!(
            "✅ {}",
            message.unwrap_or_else(|| "Operation accepted".to_owned())
        );
    } else {
        println!("❌ {failure}");
    }
}

pub fn format_timestamp(timestamp: Option<prost_types::Timestamp>) -> String {
    use time::format_description::well_known::Rfc3339;

    match timestamp {
        Some(timestamp) => {
            let nanos = i128::from(timestamp.seconds) * 1_000_000_000
                + i128::from(timestamp.nanos);
            match time::OffsetDateTime::from_unix_timestamp_nanos(nanos) {
                Ok(datetime) => datetime
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "Invalid timestamp".into()),
                Err(_) => "Invalid timestamp".into(),
            }
        }
        None => "N/A".into(),
    }
}

pub fn format_duration(duration: prost_types::Duration) -> String {
    if duration.nanos == 0 {
        format!("{}s", duration.seconds)
    } else {
        format!("{}.{:09}s", duration.seconds, duration.nanos.abs())
    }
}

#[cfg(test)]
mod tests {
    use tonic::{Code, Status};

    use super::CliError;

    #[test]
    fn grpc_status_uses_server_message_when_present() {
        let error = CliError::from(Status::permission_denied(
            "certificate access denied",
        ));

        assert_eq!(format!("{error:?}"), "certificate access denied");
    }

    #[test]
    fn grpc_status_without_message_still_identifies_failure() {
        let error = CliError::from(Status::new(Code::Unimplemented, ""));

        assert_eq!(format!("{error:?}"), "gRPC request failed (Unimplemented)");
    }
}

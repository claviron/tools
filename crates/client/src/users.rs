//! Schema-driven user management over the server's existing gRPC contract.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tonic::transport::Channel;

use crate::common::CommandResult;
use crate::proto::{management, users as wire};
use wire::user_management_client::UserManagementClient;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Show the server-owned user schema, forms and supported operations
    Schema,
    /// Read one bounded page; pass the returned cursor to continue
    List {
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(
            long,
            help = "Server-defined search text within the caller's scope"
        )]
        query: Option<String>,
    },
    /// Read a user by its opaque public reference
    Get { reference: String },
    /// Create from a JSON object matching the server's current create form
    Create {
        #[arg(long)]
        schema_revision: String,
        #[arg(long, value_name = "JSON_FILE")]
        data: PathBuf,
    },
    /// Replace only the supplied fields, using explicit concurrency revisions
    Update {
        reference: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        schema_revision: String,
        #[arg(long, value_name = "JSON_FILE")]
        data: PathBuf,
    },
    /// Delete using the revision returned by get/list
    Delete {
        reference: String,
        #[arg(long)]
        revision: String,
    },
}

pub async fn run(
    channel: Channel,
    command: Command,
) -> CommandResult {
    let mut client = UserManagementClient::new(channel);
    let output = match command {
        Command::Schema => {
            let schema = client
                .get_schema(wire::GetSchemaRequest {
                    resource: wire::Resource::User as i32,
                })
                .await?
                .into_inner();
            schema_json(schema)?
        }
        Command::List { page_size, cursor, query } => {
            let page = client
                .list_records(wire::ListRecordsRequest {
                    resource: wire::Resource::User as i32,
                    page_size,
                    cursor,
                    query,
                })
                .await?
                .into_inner();
            json!({
                "records": page.records.into_iter().map(record_json).collect::<Result<Vec<_>, _>>()?,
                "next_cursor": page.next_cursor,
            })
        }
        Command::Get { reference } => {
            let record = client
                .get_record(wire::GetRecordRequest {
                    key: Some(key(reference)?),
                })
                .await?
                .into_inner();
            record_json(record)?
        }
        Command::Create { schema_revision, data } => {
            let result = client
                .create_record(wire::CreateRecordRequest {
                    resource: wire::Resource::User as i32,
                    schema_revision: required(
                        schema_revision,
                        "schema revision",
                    )?,
                    data_json: read_object(&data)?,
                })
                .await?
                .into_inner();
            mutation_json(result)?
        }
        Command::Update { reference, revision, schema_revision, data } => {
            let result = client
                .update_record(wire::UpdateRecordRequest {
                    key: Some(key(reference)?),
                    revision: required(revision, "record revision")?,
                    schema_revision: required(
                        schema_revision,
                        "schema revision",
                    )?,
                    changes_json: read_object(&data)?,
                })
                .await?
                .into_inner();
            mutation_json(result)?
        }
        Command::Delete { reference, revision } => {
            let key = key(reference)?;
            client
                .delete_record(wire::DeleteRecordRequest {
                    key: Some(key.clone()),
                    revision: required(revision, "record revision")?,
                })
                .await?;
            json!({"deleted": key.reference})
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn required(
    value: String,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty").into());
    }
    Ok(value)
}

fn key(
    reference: String
) -> Result<wire::RecordKey, Box<dyn std::error::Error>> {
    Ok(wire::RecordKey {
        resource: wire::Resource::User as i32,
        reference: required(reference, "user reference")?,
    })
}

fn object(bytes: &[u8]) -> Result<Value, Box<dyn std::error::Error>> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| "management data must contain one valid JSON object")?;
    if !value.is_object() {
        return Err("management data must contain one JSON object".into());
    }
    Ok(value)
}

fn read_object(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    object(&bytes)?;
    // Preserve exact numbers and the submitted sparse document on the wire.
    Ok(bytes)
}

fn form_json(
    form: management::Form
) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(json!({
        "data_schema": object(&form.data_schema_json)?,
        "ui_schema": form.ui_schema_json.as_deref().map(object).transpose()?,
    }))
}

fn schema_json(
    schema: wire::ManagementSchema
) -> Result<Value, Box<dyn std::error::Error>> {
    if schema.resource != wire::Resource::User as i32
        || schema.revision.is_empty()
    {
        return Err("server returned an invalid user schema".into());
    }
    let operations = schema
        .operations
        .into_iter()
        .map(|operation| {
            wire::Operation::try_from(operation)
                .map(|value| value.as_str_name().to_owned())
                .map_err(|_| "server returned an unknown management operation")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "resource": "user",
        "revision": schema.revision,
        "record_schema": object(&schema.record_schema_json)?,
        "create": schema.create.map(form_json).transpose()?,
        "update": schema.update.map(form_json).transpose()?,
        "operations": operations,
    }))
}

fn record_json(
    record: wire::ManagedRecord
) -> Result<Value, Box<dyn std::error::Error>> {
    let key = record.key.ok_or("server returned a user without a key")?;
    if key.resource != wire::Resource::User as i32
        || key.reference.is_empty()
        || record.revision.is_empty()
    {
        return Err("server returned an invalid user record".into());
    }
    Ok(json!({
        "reference": key.reference,
        "revision": record.revision,
        "data": object(&record.data_json)?,
    }))
}

fn mutation_json(
    result: wire::RecordMutationResult
) -> Result<Value, Box<dyn std::error::Error>> {
    match result.outcome {
        Some(wire::record_mutation_result::Outcome::Record(record)) => {
            record_json(record)
        }
        Some(wire::record_mutation_result::Outcome::Validation(failure)) => {
            if failure.violations.is_empty() {
                return Err("server returned an empty validation failure; no mutation committed".into());
            }
            let violations: Vec<_> = failure.violations.into_iter().map(|value| {
                json!({"path": value.path, "code": value.code, "message": value.message})
            }).collect();
            Err(format!(
                "validation failed; no mutation committed: {}",
                serde_json::to_string(&violations)?
            )
            .into())
        }
        None => Err("server returned a mutation without an outcome".into()),
    }
}

#[cfg(test)]
#[path = "../tests/users.rs"]
mod tests;

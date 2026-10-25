use std::future::poll_fn;
use std::io::Write;
use std::pin::Pin;

use hyper::Request;
use hyper::body::Body as _;
use tonic::body::Body;
use tonic::codegen::Service;
use tonic::transport::Channel;

use crate::common::CommandResult;

/// Read the HTTP endpoint over the selected profile's existing mTLS channel.
pub async fn show(mut channel: Channel) -> CommandResult {
    let request = Request::get("/metrics")
        .header(hyper::header::ACCEPT, "text/plain; version=0.0.4")
        .body(Body::empty())?;
    poll_fn(|context| channel.poll_ready(context)).await?;
    let response = channel.call(request).await?;
    check_status(response.headers())?;
    if !response.status().is_success() {
        return Err(format!(
            "metrics endpoint returned HTTP {}",
            response.status()
        )
        .into());
    }

    let mut body = response.into_body();
    while let Some(frame) =
        poll_fn(|context| Pin::new(&mut body).poll_frame(context)).await
    {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            std::io::stdout().lock().write_all(data)?;
        }
        if let Some(trailers) = frame.trailers_ref() {
            check_status(trailers)?;
        }
    }
    std::io::stdout().lock().flush()?;
    Ok(())
}

/// Admission failures can retain HTTP 200 and carry their refusal as gRPC status.
fn check_status(headers: &hyper::HeaderMap) -> CommandResult {
    if let Some(status) = tonic::Status::from_header_map(headers)
        && status.code() != tonic::Code::Ok
    {
        return Err(status.into());
    }
    Ok(())
}

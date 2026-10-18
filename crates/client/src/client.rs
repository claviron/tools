use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::Notify;
use tonic::codegen::Service;
use tonic::codegen::http::Uri;
use tonic::transport::Channel;

#[derive(Debug, serde::Deserialize, Clone)]
pub struct ClientConfig {
    pub server_addr: String,
    pub ca_cert_path: Option<PathBuf>,
    pub client_cert_path: Option<PathBuf>,
    pub client_key_path: Option<PathBuf>,
}

impl ClientConfig {
    pub fn resolve_paths_relative_to(
        &mut self,
        config_path: &Path,
    ) {
        let base = config_path.parent().unwrap_or_else(|| Path::new("."));

        for path in [
            &mut self.ca_cert_path,
            &mut self.client_cert_path,
            &mut self.client_key_path,
        ] {
            if let Some(path) = path
                && path.is_relative()
            {
                *path = base.join(&*path);
            }
        }
    }
}

pub type ClientError = Box<dyn std::error::Error + Send + Sync>;

pub async fn create_channel(
    cfg: &ClientConfig
) -> Result<Channel, ClientError> {
    create_monitored_channel(cfg).await.map(|(channel, _)| channel)
}

pub async fn create_monitored_channel(
    cfg: &ClientConfig
) -> Result<(Channel, ConnectionMonitor), ClientError> {
    install_default_crypto_provider();

    let endpoint = Channel::from_shared(cfg.server_addr.clone())
        .map_err(|err| format!("invalid server address: {err}"))?
        .connect_timeout(std::time::Duration::from_secs(5));

    let uri = endpoint.uri().clone();
    let monitor = ConnectionMonitor::default();
    let channel = match uri.scheme_str() {
        Some("https") => {
            let tls_config = build_tls_config(cfg)?;
            let connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls_config)
                .https_only()
                .enable_http2()
                .build();

            endpoint
                .connect_with_connector(MonitoredConnector::new(
                    connector,
                    monitor.clone(),
                ))
                .await
                .map_err(|err| {
                    format!("failed to connect to {} : {err}", cfg.server_addr)
                })?
        }
        _ => {
            let connector = HttpConnector::new();
            endpoint
                .connect_with_connector(MonitoredConnector::new(
                    connector,
                    monitor.clone(),
                ))
                .await
                .map_err(|err| {
                    format!("failed to connect to {} : {err}", cfg.server_addr)
                })?
        }
    };

    Ok((channel, monitor))
}

/// Observes closures of transport connections created for one gRPC channel.
#[derive(Clone, Default)]
pub struct ConnectionMonitor {
    state: Arc<ConnectionState>,
}

#[derive(Default)]
struct ConnectionState {
    closed: AtomicU64,
    changed: Notify,
}

impl ConnectionMonitor {
    /// Snapshot the number of transport connections closed so far.
    pub fn closure_count(&self) -> u64 {
        self.state.closed.load(Ordering::Acquire)
    }

    /// Wait until a connection created by this channel closes after `observed`.
    pub async fn wait_for_close_after(
        &self,
        observed: u64,
    ) {
        loop {
            let changed = self.state.changed.notified();
            if self.closure_count() > observed {
                return;
            }
            changed.await;
        }
    }

    fn connection_closed(&self) {
        self.state.closed.fetch_add(1, Ordering::AcqRel);
        self.state.changed.notify_waiters();
    }
}

#[derive(Clone)]
struct MonitoredConnector<C> {
    inner: C,
    monitor: ConnectionMonitor,
}

impl<C> MonitoredConnector<C> {
    fn new(
        inner: C,
        monitor: ConnectionMonitor,
    ) -> Self {
        Self { inner, monitor }
    }
}

impl<C> Service<Uri> for MonitoredConnector<C>
where
    C: Service<Uri> + Send + Unpin + 'static,
    C::Response: Send + Unpin + 'static,
    C::Future: Send + 'static,
{
    type Response = MonitoredIo<C::Response>;
    type Error = C::Error;
    type Future = Pin<
        Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner).poll_ready(cx)
    }

    fn call(
        &mut self,
        request: Uri,
    ) -> Self::Future {
        let future = self.inner.call(request);
        let monitor = self.monitor.clone();
        Box::pin(async move {
            future.await.map(|inner| MonitoredIo { inner, monitor })
        })
    }
}

struct MonitoredIo<T> {
    inner: T,
    monitor: ConnectionMonitor,
}

impl<T> Drop for MonitoredIo<T> {
    fn drop(&mut self) {
        self.monitor.connection_closed();
    }
}

impl<T> Read for MonitoredIo<T>
where
    T: Read + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<T> Write for MonitoredIo<T>
where
    T: Write + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, buffers)
    }
}

fn build_tls_config(
    cfg: &ClientConfig
) -> Result<rustls::ClientConfig, ClientError> {
    let ca_path = cfg.ca_cert_path.as_deref().ok_or_else(|| {
        format!(
            "https gRPC endpoint requires ca_cert_path: {}",
            cfg.server_addr
        )
    })?;

    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(load_certs(ca_path)?);

    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|err| {
        format!("failed to create rustls client config builder: {err}")
    })?
    .with_root_certificates(roots);

    match (&cfg.client_cert_path, &cfg.client_key_path) {
        (Some(cert_path), Some(key_path)) => {
            let certs = load_certs(cert_path)?;
            let key = load_key(key_path)?;
            builder.with_client_auth_cert(certs, key).map_err(|err| {
                format!("failed to create mTLS client config: {err}").into()
            })
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(
            "client_cert_path and client_key_path must be configured together"
                .into(),
        ),
    }
}

fn load_certs(
    path: &Path
) -> Result<Vec<CertificateDer<'static>>, ClientError> {
    CertificateDer::pem_file_iter(path)
        .map_err(|err| {
            format!("failed to read certificate {}: {err}", path.display())
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            format!("failed to parse certificate {}: {err}", path.display())
                .into()
        })
}

fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, ClientError> {
    PrivateKeyDer::from_pem_file(path).map_err(|err| {
        format!("failed to read key file {}: {err}", path.display()).into()
    })
}

pub fn install_default_crypto_provider() {
    let _ = aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_paths_are_resolved_relative_to_the_config_file() {
        let mut config = ClientConfig {
            server_addr: "https://localhost:50051".to_string(),
            ca_cert_path: Some(PathBuf::from("certs/ca.pem")),
            client_cert_path: Some(PathBuf::from("certs/client.pem")),
            client_key_path: Some(PathBuf::from("/run/secrets/client.key")),
        };

        config
            .resolve_paths_relative_to(Path::new("/etc/claviron/client.yaml"));

        assert_eq!(
            config.ca_cert_path,
            Some(PathBuf::from("/etc/claviron/certs/ca.pem"))
        );
        assert_eq!(
            config.client_cert_path,
            Some(PathBuf::from("/etc/claviron/certs/client.pem"))
        );
        assert_eq!(
            config.client_key_path,
            Some(PathBuf::from("/run/secrets/client.key"))
        );
    }
}

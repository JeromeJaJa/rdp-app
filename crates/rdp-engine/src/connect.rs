//! Connection establishment: TCP → X.224 negotiation → TLS → CredSSP (NLA) → activation.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use ironrdp_connector::{ConnectionResult, Credentials, DesktopSize};
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_pdu::gcc::KeyboardType;
use ironrdp_pdu::rdp::capability_sets::{MajorPlatformType, RailSupportLevel, client_codecs_capabilities};
use ironrdp_pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use parking_lot::Mutex;

use crate::certs::{self, CertIssue, CertPolicy, CertificateStore};
use crate::gfx::{self, GfxStats, SharedOutput};
use crate::{AudioMode, CodecPreference, Command, ConnectParams, Viewport};

pub(crate) trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite {}

pub(crate) type UpgradedFramed = ironrdp_tokio::TokioFramed<Box<dyn AsyncReadWrite + Unpin + Send + Sync>>;

const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Connected {
    pub result: ConnectionResult,
    pub framed: UpgradedFramed,
}

pub(crate) fn connector_config(params: &ConnectParams, viewport: Viewport) -> ironrdp_connector::Config {
    let (username, domain) = split_user(&params.username, params.domain.as_deref());
    let (width, height) = clamp_size(viewport.width, viewport.height);

    ironrdp_connector::Config {
        credentials: Credentials::UsernamePassword {
            username,
            password: params.password.clone(),
        },
        domain,
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        connection_type: ironrdp_pdu::gcc::ConnectionType::Lan,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: DesktopSize { width, height },
        monitor_layout: None,
        desktop_scale_factor: viewport.scale.clamp(100, 500),
        bitmap: Some(ironrdp_connector::BitmapConfig {
            color_depth: 32,
            lossy_compression: true,
            codecs: client_codecs_capabilities(&[]).expect("default codec list is valid"),
        }),
        client_build: 22631,
        client_name: client_name(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        license_cache: None,
        enable_server_pointer: true,
        autologon: false,
        enable_audio_playback: params.audio != AudioMode::Off,
        enable_audio_capture: params.microphone,
        request_data: None,
        pointer_software_rendering: false,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: params.codec != CodecPreference::RemoteFx,
        compression_type: Some(CompressionType::K64),
        performance_flags: PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
    }
}

/// Everything a connection attempt needs besides the parameters.
pub(crate) struct ConnectContext {
    pub gfx_stats: Arc<GfxStats>,
    pub gfx_output: SharedOutput,
    pub clipboard: Option<Box<dyn ironrdp_cliprdr::backend::CliprdrBackend>>,
    /// Playback gate (see `audio`).
    pub audible: Arc<std::sync::atomic::AtomicBool>,
    /// The session's command queue (microphone uplink).
    pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub cert_store: Option<Arc<dyn CertificateStore>>,
    /// Set when a certificate was refused (so the session can ask the user).
    pub cert_issue: Arc<Mutex<Option<CertIssue>>>,
}

type Erased = Box<dyn AsyncReadWrite + Unpin + Send + Sync>;

/// Opens the transport: direct TCP, or a tunnel through an RD Gateway.
async fn open_transport(params: &ConnectParams, ctx: &ConnectContext) -> anyhow::Result<(Erased, std::net::SocketAddr)> {
    if let Some(gw) = &params.gateway {
        let endpoint = if gw.host.contains(':') && !gw.host.starts_with('[') {
            gw.host.clone()
        } else {
            format!("{}:443", gw.host)
        };
        let gw_user = match gw.domain.as_deref().filter(|d| !d.is_empty()) {
            Some(d) if !gw.username.contains('\\') && !gw.username.contains('@') => format!("{d}\\{}", gw.username),
            _ => gw.username.clone(),
        };
        let target = ironrdp_mstsgu::GwConnectTarget {
            gw_endpoint: endpoint,
            gw_user,
            gw_pass: gw.password.clone(),
            smart_card: None,
            server: params.host.trim_start_matches('[').trim_end_matches(']').to_owned(),
        };
        let (validation, callback) = match (&ctx.cert_store, params.cert_policy) {
            (Some(store), policy) if policy != CertPolicy::Ignore => (
                ironrdp_tls::CertificateValidation::Strict,
                Some(certs::callback(Arc::clone(store), policy, Arc::clone(&ctx.cert_issue))),
            ),
            _ => (ironrdp_tls::CertificateValidation::DangerouslyAcceptInvalidCertificate, None),
        };
        let (stream, client_addr) = tokio::time::timeout(
            Duration::from_secs(30),
            ironrdp_mstsgu::GwClient::connect_with_port_and_certificate_validation(
                &target,
                &client_name(),
                params.port,
                validation,
                callback,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("RD Gateway connect timed out"))?
        .map_err(|e| anyhow::anyhow!("RD Gateway: {e}"))?;
        return Ok((Box::new(stream), client_addr));
    }

    let addr = format!("{}:{}", host_for_socket(&params.host), params.port);
    let stream = tokio::time::timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("TCP connect timed out"))?
        .with_context(|| format!("TCP connect to {addr}"))?;
    let _ = stream.set_nodelay(true);
    let client_addr = stream.local_addr().context("local socket address")?;
    Ok((Box::new(stream), client_addr))
}

pub(crate) async fn connect(params: &ConnectParams, viewport: Viewport, ctx: ConnectContext) -> anyhow::Result<Connected> {
    let (stream, client_addr) = open_transport(params, &ctx).await?;

    let mut drdynvc = DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    if let Some(gfx) = gfx::build(params, Arc::clone(&ctx.gfx_stats), Arc::clone(&ctx.gfx_output)) {
        drdynvc = drdynvc.with_dynamic_channel(gfx);
    }
    if params.microphone {
        drdynvc = drdynvc.with_listener(crate::audio::MicrophoneListener {
            tx: ctx.commands.clone(),
        });
    }

    let config = connector_config(params, viewport);
    let mut connector = ironrdp_connector::ClientConnector::new(config, client_addr).with_static_channel(drdynvc);
    if let Some(backend) = ctx.clipboard {
        connector.attach_static_channel(ironrdp_cliprdr::Cliprdr::new(backend));
    }
    let rdpdr = match crate::devices::rdpdr(&params.devices) {
        Ok(channel) => channel,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "device redirection unavailable");
            None
        }
    };
    let playback = params.audio != AudioMode::Off;
    if playback || rdpdr.is_some() {
        // Windows expects RDPSND whenever RDPDR is present.
        connector.attach_static_channel(crate::audio::rdpsnd(playback, Arc::clone(&ctx.audible)));
    }
    if let Some(rdpdr) = rdpdr {
        connector.attach_static_channel(rdpdr);
    }

    let mut framed = ironrdp_tokio::TokioFramed::new(stream);
    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector)
        .await
        .context("RDP negotiation")?;

    let (initial_stream, leftover) = framed.into_inner();
    let tls_name = params.host.trim_start_matches('[').trim_end_matches(']');
    let endpoint = format!("{}:{}", host_for_socket(&params.host), params.port);
    let tls = match (&ctx.cert_store, params.cert_policy) {
        (Some(store), policy) if policy != CertPolicy::Ignore => {
            let callback = certs::callback(Arc::clone(store), policy, Arc::clone(&ctx.cert_issue));
            ironrdp_tls::upgrade_with_certificate_validation_callback_for_endpoint(
                initial_stream,
                tls_name,
                &endpoint,
                callback,
            )
            .await
        }
        _ => {
            ironrdp_tls::upgrade_with_certificate_validation(
                initial_stream,
                tls_name,
                ironrdp_tls::CertificateValidation::DangerouslyAcceptInvalidCertificate,
            )
            .await
        }
    };
    let (tls_stream, tls_cert) = tls.context("TLS handshake")?;

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let erased: Box<dyn AsyncReadWrite + Unpin + Send + Sync> = Box::new(tls_stream);
    let mut framed = ironrdp_tokio::TokioFramed::new_with_leftover(erased, leftover);

    let server_public_key = ironrdp_tls::extract_tls_server_public_key(&tls_cert)
        .context("server certificate has no public key")?
        .to_owned();

    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut ReqwestNetworkClient::new(),
        tls_name.to_owned().into(),
        server_public_key,
        None,
    )
    .await
    .context("authentication and session setup")?;

    Ok(Connected { result, framed })
}

/// Accepts `DOMAIN\user` and `user@domain` forms; an explicit domain wins.
fn split_user(raw: &str, domain: Option<&str>) -> (String, Option<String>) {
    let explicit = domain.map(str::trim).filter(|d| !d.is_empty()).map(str::to_owned);
    if let Some((d, u)) = raw.split_once('\\') {
        return (
            u.to_owned(),
            explicit.or_else(|| Some(d.to_owned()).filter(|d| !d.is_empty())),
        );
    }
    (raw.to_owned(), explicit)
}

/// Desktop size limits from MS-RDPBCGR: width even, 200..=8192.
pub(crate) fn clamp_size(width: u16, height: u16) -> (u16, u16) {
    let w = width.clamp(200, 8192) & !1;
    let h = height.clamp(200, 8192);
    (w, h)
}

fn host_for_socket(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn client_name() -> String {
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "rdp-app".into());
    name.chars().take(15).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_domain_forms() {
        assert_eq!(split_user("CORP\\alice", None), ("alice".into(), Some("CORP".into())));
        assert_eq!(split_user("alice", Some("CORP")), ("alice".into(), Some("CORP".into())));
        assert_eq!(
            split_user("CORP\\alice", Some("OTHER")),
            ("alice".into(), Some("OTHER".into()))
        );
        assert_eq!(split_user("alice@corp.local", None), ("alice@corp.local".into(), None));
        assert_eq!(split_user("alice", Some("  ")), ("alice".into(), None));
    }

    #[test]
    fn clamps_desktop_size() {
        assert_eq!(clamp_size(1921, 100), (1920, 200));
        assert_eq!(clamp_size(9000, 9000), (8192, 8192));
    }
}

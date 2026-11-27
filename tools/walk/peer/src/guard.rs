//! Transport guard: every connection must go to an official Telegram TEST data center.
//! Same rules as the `test_dc_smoke` harness in `tg-session/examples`.

use std::collections::HashSet;

use parking_lot::RwLock;
use tg_mtproto::dc_options::{DcId, DcOptions, DcType, Environment as MtpEnvironment};
use tg_mtproto::transport::{Error as TransportError, ProxyData};
use tg_mtproto::transport_adapter::BoxFuture;
use tg_mtproto::{
    ClientConfig, PacketConnection, TgTransportFactory, TransportFactory, build_race_candidates,
};
use tg_tl::{enums as tl_enums, types as tl_types};

/// Production DC addresses that must never be dialed.
const PRODUCTION_DC_IPS: &[&str] = &[
    "149.154.175.50",
    "149.154.167.51",
    "149.154.167.50",
    "95.161.76.100",
    "149.154.175.100",
    "149.154.167.91",
    "149.154.167.92",
    "149.154.171.5",
    "2001:0b28:f23d:f001:0000:0000:0000:000a",
    "2001:067c:04e8:f002:0000:0000:0000:000a",
    "2001:0b28:f23d:f003:0000:0000:0000:000a",
    "2001:067c:04e8:f004:0000:0000:0000:000a",
    "2001:0b28:f23f:f005:0000:0000:0000:000a",
];

/// Built-in test DC endpoints (`mtproto_dc_options.cpp::kBuiltInDcsTest`).
const INITIAL_TEST_DC_IPS: &[&str] = &[
    "149.154.175.10",
    "149.154.167.40",
    "149.154.175.117",
    "2001:0b28:f23d:f001:0000:0000:0000:000e",
    "2001:067c:04e8:f002:0000:0000:0000:000e",
    "2001:0b28:f23d:f003:0000:0000:0000:000e",
];

fn is_production_ip(ip: &str) -> bool {
    let trimmed = ip.trim();
    PRODUCTION_DC_IPS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(trimmed))
}

/// Refuses any endpoint that is not a known test DC.
pub struct TestDcGuard {
    inner: TgTransportFactory,
    allowed_ips: RwLock<HashSet<String>>,
    allowed_endpoints: RwLock<HashSet<(String, u16)>>,
}

impl TestDcGuard {
    pub fn new() -> Self {
        let mut ips = HashSet::new();
        let mut eps = HashSet::new();
        for &ip in INITIAL_TEST_DC_IPS {
            ips.insert(ip.to_owned());
            eps.insert((ip.to_owned(), 443));
            eps.insert((ip.to_owned(), 80));
        }
        Self {
            inner: TgTransportFactory::default(),
            allowed_ips: RwLock::new(ips),
            allowed_endpoints: RwLock::new(eps),
        }
    }

    /// Accepts the DC list of a `help.getConfig` answer, which must say `test_mode`.
    pub fn register_config(&self, cfg: &tl_types::Config) -> Result<(), String> {
        if !cfg.test_mode {
            return Err("SAFETY: help.getConfig says test_mode=false; refusing".into());
        }
        let mut ips = self.allowed_ips.write();
        let mut eps = self.allowed_endpoints.write();
        for opt in &cfg.dc_options {
            let tl_enums::DcOption::DcOption(opt) = opt;
            if is_production_ip(&opt.ip_address) {
                return Err("SAFETY: test config lists a production DC address".into());
            }
            if let Ok(port) = u16::try_from(opt.port) {
                ips.insert(opt.ip_address.clone());
                eps.insert((opt.ip_address.clone(), port));
            }
        }
        Ok(())
    }

    fn check(&self, ip: &str, port: u16) -> Result<(), TransportError> {
        let ok = !is_production_ip(ip)
            && self.allowed_ips.read().contains(ip)
            && (port == 443
                || port == 80
                || self
                    .allowed_endpoints
                    .read()
                    .contains(&(ip.to_owned(), port)));
        if ok {
            Ok(())
        } else {
            eprintln!("SAFETY: refused a connection to a non-test endpoint");
            Err(TransportError::Disconnected)
        }
    }
}

impl TransportFactory for TestDcGuard {
    fn connect<'a>(
        &'a self,
        ip: &'a str,
        port: u16,
        protocol_dc_id: i16,
        secret: &'a [u8],
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            self.check(ip, port)?;
            if protocol_dc_id.abs() < 10_000 {
                return Err(TransportError::Disconnected);
            }
            self.inner.connect(ip, port, protocol_dc_id, secret).await
        })
    }

    fn connect_dc<'a>(
        &'a self,
        bare_dc: DcId,
        dc_type: DcType,
        dc_options: &'a DcOptions,
        config: &'a ClientConfig,
        proxy: &'a ProxyData,
    ) -> BoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            if !dc_options.is_test_mode() || config.environment != MtpEnvironment::Test {
                return Err(TransportError::Disconnected);
            }
            for cand in build_race_candidates(bare_dc, dc_type, dc_options, config, proxy) {
                match &cand.endpoint {
                    tg_transport::RaceEndpoint::Tcp {
                        address,
                        port,
                        config: c,
                    } => {
                        self.check(address, *port)?;
                        if c.dc_id.abs() < 10_000 {
                            return Err(TransportError::Disconnected);
                        }
                    }
                    tg_transport::RaceEndpoint::Http { address, config: c } => {
                        self.check(address, c.port)?;
                    }
                }
            }
            self.inner
                .connect_dc(bare_dc, dc_type, dc_options, config, proxy)
                .await
        })
    }
}

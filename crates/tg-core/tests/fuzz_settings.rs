//! Corrupt settings blobs must give an error, never a panic or an allocation abort (TGB-3060).

use std::panic::{AssertUnwindSafe, catch_unwind};

use tg_core::local_url::{parse_local_url, parse_proxy_link, try_convert_url_to_local};
use tg_core::sandbox::{SingleInstanceResponse, parse_command_batch};
use tg_core::settings::AccentColors;
use tg_core::settings_proxy::{ProxyData, SettingsProxy};
use tg_core::{CoreSettings, QDataStreamWriter};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

const INTERESTING: [u32; 6] = [0, 1, 0x7FFF, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF];

/// Overwrites 1-3 aligned 32-bit fields of `seed` with interesting values.
fn mutate(rng: &mut Lcg, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    for _ in 0..=rng.below(3) {
        if out.len() < 4 {
            break;
        }
        let i = rng.below(out.len() / 4) * 4;
        let v = INTERESTING[rng.below(INTERESTING.len())];
        out[i..i + 4].copy_from_slice(&v.to_be_bytes());
    }
    if rng.below(4) == 0 {
        let cut = rng.below(out.len() + 1);
        out.truncate(cut);
    }
    out
}

fn no_panics(name: &str, seed: &[u8], iterations: usize, decode: impl Fn(&[u8])) {
    let mut rng = Lcg(0xC0DE ^ seed.len() as u64);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failure = None;
    for _ in 0..iterations {
        let input = mutate(&mut rng, seed);
        if catch_unwind(AssertUnwindSafe(|| decode(&input))).is_err() {
            failure = Some(input);
            break;
        }
    }
    std::panic::set_hook(previous_hook);
    if let Some(input) = failure {
        panic!("{name} panicked on {input:02x?}");
    }
}

/// The proxy list count used to size a `Vec` directly: `0x7FFF_FFFF` asked for ~200 GB and the
/// failed allocation aborted the app while it read its settings at startup.
#[test]
fn huge_proxy_list_count_is_an_error_not_an_abort() {
    let mut w = QDataStreamWriter::new();
    w.write_i32(0); // try_ipv6
    w.write_i32(0); // use_proxy_for_calls
    w.write_i32(0); // settings mode
    w.write_byte_array(&ProxyData::default().serialize());
    w.write_i32(i32::MAX); // list count, no entries follow
    let mut proxy = SettingsProxy::default();
    assert!(proxy.set_from_serialized(w.as_bytes()).is_err());
}

#[test]
fn core_settings_blob_never_panics_or_aborts() {
    let seed = CoreSettings::new().serialize();
    no_panics("core_settings", &seed, 4_000, |bytes| {
        let mut settings = CoreSettings::new();
        let _ = settings.add_from_serialized(bytes, 6_000_000);
    });
}

#[test]
fn proxy_and_accent_blobs_never_panic_or_abort() {
    let mut proxy = SettingsProxy::default();
    proxy.add_to_list(ProxyData::default());
    let seed = proxy.serialize();
    no_panics("settings_proxy", &seed, 8_000, |bytes| {
        let _ = SettingsProxy::default().set_from_serialized(bytes);
    });
    no_panics("accent_colors", &seed, 8_000, |bytes| {
        let _ = AccentColors::default().set_from_serialized(bytes);
    });
}

#[test]
fn links_and_instance_messages_never_panic() {
    let mut rng = Lcg(7);
    let alphabet: Vec<char> = "tg://resolve?domain=a&start=b/+%😀é:;_RES0123456789CMD:OPEN:"
        .chars()
        .collect();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failure = None;
    for _ in 0..20_000 {
        let len = rng.below(48);
        let text: String = (0..len)
            .map(|_| alphabet[rng.below(alphabet.len())])
            .collect();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = try_convert_url_to_local(&text);
            let _ = parse_local_url(&text);
            let _ = parse_proxy_link(&text);
            let _ = parse_command_batch(&text);
            let _ = SingleInstanceResponse::decode(&text);
        }));
        if result.is_err() {
            failure = Some(text);
            break;
        }
    }
    std::panic::set_hook(previous_hook);
    if let Some(text) = failure {
        panic!("link/instance parsing panicked on {text:?}");
    }
}

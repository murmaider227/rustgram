//! OS process sandboxing (Win32 restricted tokens, Job objects, process mitigation policies,
//! owner-DACL named pipe channels via `windows-sys`) and WebView2 HTML widget rendering sandbox
//! built on `tg-webview`.
//!
//! Port origin: `Telegram/SourceFiles/platform/win/widget_sandbox_win.h/.cpp`,
//! `platform/win/widget_channel_win.h/.cpp`, `platform/win/widget_helper_win.cpp`,
//! `_other/widget_helper_win.cpp`, and `tg-webview`.

use std::sync::Arc;

use tg_webview::{
    Availability, SchemeRouter, WebView, WebViewBounds, WebViewCallbacks, WebViewConfig, WindowMode,
};
use url::Url;

use crate::error::{Result, TgwError};
use crate::model::WidgetTheme;

/// AppContainer profile name used for the sandboxed widget helper (`kAppContainerName`).
pub const APP_CONTAINER_NAME: &str = "telegram.desktop.widget-helper";

/// Security attribute identifying a less-privileged AppContainer (`kLessPrivilegedName`).
pub const LESS_PRIVILEGED_ATTR: &str = "WIN://NOALLAPPPKG";

/// Custom URL scheme used when hosting sandboxed HTML widgets inside WebView2.
pub const WIDGET_CUSTOM_SCHEME: &str = "tgw-widget";

/// Default entry document ID served over `tgw-widget://`.
pub const WIDGET_ENTRY_DOC_ID: &str = "widget/index.html";

/// Named pipe buffer size (64 KiB, `kBufferSize` in `widget_channel_win.cpp:21`).
pub const CHANNEL_PIPE_BUFFER_BYTES: u32 = 64 * 1024;

// Process creation mitigation policy bits matching WinSDK `winbase.h` (`ComputePolicies` in
// `widget_sandbox_win.cpp:557-650`).
pub const MITIGATION_DEP_ENABLE: u64 = 0x0000_0000_0000_0001;
pub const MITIGATION_FORCE_RELOCATE_IMAGES_ALWAYS_ON: u64 = 0x0000_0000_0000_0100;
pub const MITIGATION_BOTTOM_UP_ASLR_ALWAYS_ON: u64 = 0x0000_0000_0001_0000;
pub const MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON: u64 = 0x0000_0000_0010_0000;
pub const MITIGATION_STRICT_HANDLE_CHECKS_ALWAYS_ON: u64 = 0x0000_0000_0100_0000;
pub const MITIGATION_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON: u64 = 0x0000_0000_1000_0000;
pub const MITIGATION_EXTENSION_POINT_DISABLE_ALWAYS_ON: u64 = 0x0000_0001_0000_0000;
pub const MITIGATION_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON: u64 = 0x0000_0010_0000_0000;
pub const MITIGATION_FONT_DISABLE_ALWAYS_ON: u64 = 0x0001_0000_0000_0000;
pub const MITIGATION_BLOCK_NON_MICROSOFT_BINARIES_ALWAYS_ON: u64 = 0x0000_1000_0000_0000;
pub const MITIGATION_IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON: u64 = 1u64 << 60;
pub const MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON: u64 = 1u64 << 28;

/// `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED` (`winbase.h`).
pub const CHILD_PROCESS_RESTRICTED: u32 = 0x01;

/// `PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT` (`winbase.h`).
pub const ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 0x01;

/// Job extended limit flags (`kJobLimits` in `widget_sandbox_win.cpp:55-57`).
pub const JOB_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
pub const JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
pub const JOB_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
pub const DEFAULT_JOB_LIMIT_FLAGS: u32 =
    JOB_LIMIT_ACTIVE_PROCESS | JOB_LIMIT_KILL_ON_JOB_CLOSE | JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;

/// Job UI restriction flags (`kJobUiRestrictions` in `widget_sandbox_win.cpp:59-66`).
pub const JOB_UILIMIT_HANDLES: u32 = 0x0000_0001;
pub const JOB_UILIMIT_READCLIPBOARD: u32 = 0x0000_0002;
pub const JOB_UILIMIT_WRITECLIPBOARD: u32 = 0x0000_0004;
pub const JOB_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0000_0008;
pub const JOB_UILIMIT_DISPLAYSETTINGS: u32 = 0x0000_0010;
pub const JOB_UILIMIT_GLOBALATOMS: u32 = 0x0000_0020;
pub const JOB_UILIMIT_DESKTOP: u32 = 0x0000_0040;
pub const JOB_UILIMIT_EXITWINDOWS: u32 = 0x0000_0080;
pub const DEFAULT_JOB_UI_RESTRICTIONS: u32 = JOB_UILIMIT_HANDLES
    | JOB_UILIMIT_READCLIPBOARD
    | JOB_UILIMIT_WRITECLIPBOARD
    | JOB_UILIMIT_SYSTEMPARAMETERS
    | JOB_UILIMIT_DISPLAYSETTINGS
    | JOB_UILIMIT_GLOBALATOMS
    | JOB_UILIMIT_DESKTOP
    | JOB_UILIMIT_EXITWINDOWS;

/// `SeChangeNotifyPrivilege` LUID low part (`kChangeNotifyPrivilege` in `widget_sandbox_win.cpp:53`).
pub const SE_CHANGE_NOTIFY_LUID_LOW: u32 = 23;

/// AppContainer privilege tier (`Platform::WidgetHelper::AppContainerLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppContainerLevel {
    /// Regular AppContainer (includes `ALL APPLICATION PACKAGES` SID).
    Regular,
    /// Less-privileged AppContainer (`WIN://NOALLAPPPKG`, strips `ALL APPLICATION PACKAGES`).
    LessPrivileged,
}

/// Process mitigation policy expectation verified inside the helper (`Platform::WidgetHelper::Mitigation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MitigationExpectation {
    /// `PROCESS_MITIGATION_POLICY` numeric identifier.
    pub policy_id: u32,
    /// Expected struct size in bytes.
    pub size_bytes: u32,
    /// Bits that must be set.
    pub required_flags: u32,
    /// Bits that must remain clear.
    pub forbidden_flags: u32,
}

/// Job object limits & UI restrictions policy (`widget_sandbox_win.cpp:55-66`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLimitsPolicy {
    /// Extended limit flags (`ACTIVE_PROCESS | KILL_ON_JOB_CLOSE | DIE_ON_UNHANDLED_EXCEPTION`).
    pub limit_flags: u32,
    /// Maximum active processes in the job (`1`).
    pub active_process_limit: u32,
    /// Basic UI restrictions bitmask (handles, clipboard, desktop, display, etc.).
    pub ui_restrictions: u32,
}

impl Default for JobLimitsPolicy {
    fn default() -> Self {
        Self {
            limit_flags: DEFAULT_JOB_LIMIT_FLAGS,
            active_process_limit: 1,
            ui_restrictions: DEFAULT_JOB_UI_RESTRICTIONS,
        }
    }
}

/// Restricted token construction policy (`CreateTokens` in `widget_sandbox_win.cpp:735-831`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictedTokenPolicy {
    /// Target integrity level (`"Low"`, `S-1-16-4096`).
    pub integrity_level: &'static str,
    /// Whether `DISABLE_MAX_PRIVILEGE` is applied (only `SeChangeNotifyPrivilege` may remain).
    pub disable_max_privilege: bool,
    /// Whether user and all non-integrity groups are marked `SE_GROUP_USE_FOR_DENY_ONLY`.
    pub deny_only_all_groups: bool,
    /// Whether the restricting SID list is strictly `[WinNullSid]` (`S-1-0-0`).
    pub restrict_to_null_sid: bool,
    /// AppContainer name used for LowBox derivation.
    pub app_container_name: &'static str,
    /// Target AppContainer privilege tier for the locked-down primary token.
    pub app_container_level: AppContainerLevel,
}

impl Default for RestrictedTokenPolicy {
    fn default() -> Self {
        Self {
            integrity_level: "Low",
            disable_max_privilege: true,
            deny_only_all_groups: true,
            restrict_to_null_sid: true,
            app_container_name: APP_CONTAINER_NAME,
            app_container_level: AppContainerLevel::LessPrivileged,
        }
    }
}

/// Strict Content-Security-Policy and browser API lockdown for WebView2 widget rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetCspPolicy {
    /// Content-Security-Policy header / meta directive value.
    pub csp_header: String,
    /// JavaScript init script that disables dangerous web APIs and installs the widget bridge.
    pub lockdown_script: String,
}

impl Default for WidgetCspPolicy {
    fn default() -> Self {
        Self::strict()
    }
}

impl WidgetCspPolicy {
    /// Builds the strict no-network, no-storage, inline-only CSP and API lockdown script for
    /// sandboxed HTML widget messages.
    #[must_use]
    pub fn strict() -> Self {
        let csp_header = "default-src 'none'; \
            base-uri 'none'; \
            child-src 'none'; \
            connect-src 'none'; \
            font-src 'none'; \
            form-action 'none'; \
            frame-src 'none'; \
            img-src data:; \
            manifest-src 'none'; \
            media-src 'none'; \
            object-src 'none'; \
            script-src 'unsafe-inline'; \
            style-src 'unsafe-inline'; \
            worker-src 'none'"
            .to_owned();

        let csp_literal = serde_json::to_string(&csp_header)
            .unwrap_or_else(|_| "\"default-src 'none'\"".to_owned());
        let lockdown_script = format!(
            r#"(()=>{{
const policy={csp_literal};
const applyPolicy=()=>{{
 const head=document.head;if(!head)return false;
 const dns=document.createElement('meta');
 dns.httpEquiv='x-dns-prefetch-control';dns.content='off';
 head.insertBefore(dns,head.firstChild);
 const meta=document.createElement('meta');
 meta.httpEquiv='Content-Security-Policy';meta.content=policy;
 head.insertBefore(meta,head.firstChild);
 return true;
}};
if(!applyPolicy()){{
 const obs=new MutationObserver(()=>{{if(applyPolicy())obs.disconnect()}});
 obs.observe(document,{{childList:true,subtree:true}});
}}
const lock=(obj,name,value)=>{{try{{Object.defineProperty(obj,name,{{value,configurable:false,writable:false}})}}catch(_){{}}}};
for(const n of ['fetch','XMLHttpRequest','WebSocket','EventSource','localStorage','sessionStorage','indexedDB','caches','Worker','SharedWorker','BroadcastChannel','Audio','AudioContext','webkitAudioContext','RTCPeerConnection','WebTransport','WebAssembly','Notification','PaymentRequest','MediaRecorder','showOpenFilePicker','showSaveFilePicker','showDirectoryPicker'])lock(globalThis,n,undefined);
for(const n of ['serviceWorker','clipboard','geolocation','mediaDevices','usb','serial','hid','bluetooth','credentials','getUserMedia','wakeLock','share','xr','getGamepads','storage','locks','sendBeacon','permissions'])lock(navigator,n,undefined);
lock(globalThis,'open',()=>null);
lock(globalThis,'print',()=>{{}});
lock(globalThis,'alert',()=>{{}});
lock(globalThis,'confirm',()=>false);
lock(globalThis,'prompt',()=>null);
try{{Object.defineProperty(document,'cookie',{{get:()=>'',set:()=>true,configurable:false}})}}catch(_){{}}
const post=(msg)=>{{try{{if(window.chrome&&window.chrome.webview)window.chrome.webview.postMessage(JSON.stringify(msg))}}catch(_){{}}}};
lock(globalThis,'TelegramWidgetBridge',Object.freeze({{
 ready:()=>post({{type:'ready'}}),
 resize:(height)=>post({{type:'size',height:Number(height)||0}}),
 openLink:(url)=>post({{type:'openLink',url:String(url||''),telegram:false}}),
 openTelegramLink:(url)=>post({{type:'openLink',url:String(url||''),telegram:true}}),
 copyText:(text)=>post({{type:'copyText',text:String(text||'')}}),
 query:(requestId,data,wallet)=>post({{type:'botQuery',request_id:Number(requestId)||0,data:String(data||''),wallet:Boolean(wallet)}}),
 commitStorage:(area,values)=>post({{type:'storageCommit',area:String(area||'local'),values:values||{{}}}}),
}}));
}})()"#
        );

        Self {
            csp_header,
            lockdown_script,
        }
    }
}

/// Complete OS + WebView2 sandbox policy specification (`Platform::WidgetHelper::Policies`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Process creation mitigation policy words (`[word0, word1]`).
    pub creation_flags: [u64; 2],
    /// Effective size in bytes of `creation_flags` passed to `UpdateProcThreadAttribute` (8 or 16).
    pub creation_size: usize,
    /// `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY` value.
    pub child_process_policy: u32,
    /// `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY` value.
    pub all_application_packages_policy: u32,
    /// Individual mitigation expectations verified inside the helper.
    pub mitigations: Vec<MitigationExpectation>,
    /// Job object limits and UI restrictions.
    pub job: JobLimitsPolicy,
    /// Restricted token / LowBox AppContainer policy.
    pub token: RestrictedTokenPolicy,
    /// Strict WebView2 CSP & lockdown script.
    pub csp: WidgetCspPolicy,
}

impl SandboxPolicy {
    /// Builds the default hardened sandbox policy (`ComputePolicies` in `widget_sandbox_win.cpp:557-650`).
    #[must_use]
    pub fn build(is_64bit: bool, cet_available: bool) -> Self {
        let mut words = [0u64; 2];
        let mut mitigations = Vec::new();

        if !is_64bit {
            words[0] |= MITIGATION_DEP_ENABLE;
            mitigations.push(MitigationExpectation {
                policy_id: 0, // ProcessDEPPolicy
                size_bytes: 8,
                required_flags: 0x03, // Enable | DisableAtlThunkEmulation
                forbidden_flags: 0,
            });
        }

        // ProcessASLRPolicy (1)
        words[0] |=
            MITIGATION_BOTTOM_UP_ASLR_ALWAYS_ON | MITIGATION_FORCE_RELOCATE_IMAGES_ALWAYS_ON;
        let mut aslr_req = 0x03u32; // EnableBottomUpRandomization | EnableForceRelocateImages
        if is_64bit {
            words[0] |= MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON;
            aslr_req |= 0x04; // EnableHighEntropy
        }
        mitigations.push(MitigationExpectation {
            policy_id: 1,
            size_bytes: 4,
            required_flags: aslr_req,
            forbidden_flags: 0,
        });

        // ProcessDynamicCodePolicy (2)
        words[0] |= MITIGATION_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 2,
            size_bytes: 4,
            required_flags: 0x01,  // ProhibitDynamicCode
            forbidden_flags: 0x06, // AllowThreadOptOut | AllowRemoteDowngrade
        });

        // ProcessStrictHandleCheckPolicy (3)
        words[0] |= MITIGATION_STRICT_HANDLE_CHECKS_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 3,
            size_bytes: 4,
            required_flags: 0x03,
            forbidden_flags: 0,
        });

        // ProcessSystemCallDisablePolicy (4)
        words[0] |= MITIGATION_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 4,
            size_bytes: 4,
            required_flags: 0x01, // DisallowWin32kSystemCalls
            forbidden_flags: 0,
        });

        // ProcessExtensionPointDisablePolicy (6)
        words[0] |= MITIGATION_EXTENSION_POINT_DISABLE_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 6,
            size_bytes: 4,
            required_flags: 0x01,
            forbidden_flags: 0,
        });

        // ProcessSignaturePolicy (8)
        words[0] |= MITIGATION_BLOCK_NON_MICROSOFT_BINARIES_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 8,
            size_bytes: 4,
            required_flags: 0x01, // MicrosoftSignedOnly
            forbidden_flags: 0,
        });

        // ProcessFontDisablePolicy (9)
        words[0] |= MITIGATION_FONT_DISABLE_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 9,
            size_bytes: 4,
            required_flags: 0x01, // DisableNonSystemFonts
            forbidden_flags: 0,
        });

        // ProcessImageLoadPolicy (10)
        words[0] |= MITIGATION_IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 10,
            size_bytes: 4,
            required_flags: 0x04, // PreferSystem32Images
            forbidden_flags: 0,
        });

        // ProcessChildProcessPolicy (13)
        mitigations.push(MitigationExpectation {
            policy_id: 13,
            size_bytes: 4,
            required_flags: 0x01,  // NoChildProcessCreation
            forbidden_flags: 0x04, // AllowSecureProcessCreation
        });

        // ProcessUserShadowStackPolicy (16) when CET is available on x64
        if is_64bit && cet_available {
            words[1] |= MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON;
            mitigations.push(MitigationExpectation {
                policy_id: 16,
                size_bytes: 4,
                required_flags: 0x01,
                forbidden_flags: 0,
            });
        }

        let creation_size = if words[1] != 0 { 16 } else { 8 };

        Self {
            creation_flags: words,
            creation_size,
            child_process_policy: CHILD_PROCESS_RESTRICTED,
            all_application_packages_policy: ALL_APPLICATION_PACKAGES_OPT_OUT,
            mitigations,
            job: JobLimitsPolicy::default(),
            token: RestrictedTokenPolicy::default(),
            csp: WidgetCspPolicy::strict(),
        }
    }

    /// Builds the default sandbox policy for the current target architecture.
    #[must_use]
    pub fn for_current_target() -> Self {
        let is_64bit = cfg!(target_pointer_width = "64");
        Self::build(is_64bit, false)
    }

    /// Validates that this policy satisfies every required hardening invariant.
    pub fn validate(&self) -> Result<()> {
        if (self.job.limit_flags & DEFAULT_JOB_LIMIT_FLAGS) != DEFAULT_JOB_LIMIT_FLAGS {
            return Err(TgwError::Sandbox(
                "job policy missing required active-process / kill-on-close / die-on-exception flags"
                    .to_owned(),
            ));
        }
        if self.job.active_process_limit != 1 {
            return Err(TgwError::Sandbox(format!(
                "job active_process_limit must be 1, got {}",
                self.job.active_process_limit
            )));
        }
        if (self.job.ui_restrictions & DEFAULT_JOB_UI_RESTRICTIONS) != DEFAULT_JOB_UI_RESTRICTIONS {
            return Err(TgwError::Sandbox(
                "job policy missing required UI restrictions".to_owned(),
            ));
        }
        if self.child_process_policy != CHILD_PROCESS_RESTRICTED {
            return Err(TgwError::Sandbox(
                "child process restriction policy must be enabled".to_owned(),
            ));
        }
        if !self.token.disable_max_privilege
            || !self.token.deny_only_all_groups
            || !self.token.restrict_to_null_sid
        {
            return Err(TgwError::Sandbox(
                "restricted token policy must strip privileges, deny groups, and restrict to WinNullSid"
                    .to_owned(),
            ));
        }
        if !self.csp.csp_header.contains("default-src 'none'")
            || !self.csp.csp_header.contains("connect-src 'none'")
        {
            return Err(TgwError::Sandbox(
                "widget CSP must enforce default-src 'none' and connect-src 'none'".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Quotes a command-line argument according to Windows `CommandLineToArgvW` rules
/// (`QuotedArgument` in `widget_helper_win.cpp:88-108`).
#[must_use]
pub fn quote_win32_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0B' | '"'))
    {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut slashes = 0usize;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            for _ in 0..(slashes * 2 + 1) {
                out.push('\\');
            }
        } else {
            for _ in 0..slashes {
                out.push('\\');
            }
        }
        slashes = 0;
        out.push(ch);
    }
    for _ in 0..(slashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Returns `true` if `url` is permitted to navigate inside a sandboxed HTML widget WebView2.
///
/// Only the custom widget document scheme (`tgw-widget://...`) and `about:blank` are permitted;
/// all network URLs (`http://`, `https://`, `file://`, `javascript:`) are blocked at the
/// navigation layer and must go through the host `openLink` / `openTelegramLink` bridge.
#[must_use]
pub fn is_allowed_widget_navigation(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.eq_ignore_ascii_case("about:blank") {
        return true;
    }
    let Ok(parsed) = Url::parse(trimmed) else {
        return false;
    };
    parsed.scheme().eq_ignore_ascii_case(WIDGET_CUSTOM_SCHEME)
}

/// Builds a sandboxed HTML wrapper document injecting theme CSS variables and CSP meta tags
/// around `raw_html` for WebView2 rendering.
#[must_use]
pub fn wrap_widget_html(raw_html: &str, theme: &WidgetTheme, csp: &WidgetCspPolicy) -> String {
    let mut css_vars = String::new();
    for (k, v) in &theme.params {
        css_vars.push_str("--tg-theme-");
        css_vars.push_str(&k.replace('_', "-"));
        css_vars.push(':');
        css_vars.push_str(v);
        css_vars.push(';');
    }
    let escaped_csp = csp.csp_header.replace('&', "&amp;").replace('"', "&quot;");
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"x-dns-prefetch-control\" content=\"off\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{escaped_csp}\">\
         <style>:root{{{css_vars}color-scheme:{scheme};}}html,body{{margin:0;padding:0;overflow:hidden;background:var(--tg-theme-bg-color,transparent);color:var(--tg-theme-text-color,inherit);font-family:system-ui,sans-serif;}}</style>\
         </head><body>{raw_html}</body></html>",
        scheme = theme.color_scheme
    )
}

// =============================================================================
// WebView2 Sandboxed Widget Host (built on `tg-webview`)
// =============================================================================

/// Embedded WebView2 host for a sandboxed HTML widget, enforcing strict CSP, no-network
/// navigation policy, and custom-scheme in-memory HTML delivery via `tg-webview`.
pub struct WebViewWidgetHost {
    webview: WebView,
    policy: SandboxPolicy,
}

impl WebViewWidgetHost {
    /// Queries WebView2 runtime availability via `tg-webview`.
    #[must_use]
    pub fn availability() -> Availability {
        tg_webview::availability()
    }

    /// Creates a sandboxed WebView2 widget host serving `wrapped_html` from an in-memory
    /// [`SchemeRouter`] over `tgw-widget://` with all external navigation blocked.
    pub fn create(
        parent: Option<tg_webview::WebViewBounds>,
        raw_html: &str,
        theme: &WidgetTheme,
        policy: SandboxPolicy,
        on_message: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<Self> {
        policy.validate()?;
        let wrapped = wrap_widget_html(raw_html, theme, &policy.csp);
        let mut router = SchemeRouter::new(WIDGET_CUSTOM_SCHEME);
        router.set_accept_legacy_prefix(false);
        router.add_html(WIDGET_ENTRY_DOC_ID, wrapped);
        let router = Arc::new(router);

        let callbacks = WebViewCallbacks {
            on_message: Some(Arc::new(move |msg| {
                on_message(msg.text);
            })),
            on_navigation_policy: Some(Arc::new(|url, _new_window| {
                is_allowed_widget_navigation(url)
            })),
            ..WebViewCallbacks::default()
        };

        let config = WebViewConfig {
            mode: WindowMode::Hidden,
            custom_scheme: WIDGET_CUSTOM_SCHEME.to_owned(),
            scheme_router: Some(router),
            debug: false,
            safe: true,
            callbacks,
            ..WebViewConfig::default()
        };

        let webview =
            WebView::create_from_raw(None, config).map_err(|e| TgwError::WebView(e.to_string()))?;
        webview
            .init(&policy.csp.lockdown_script)
            .map_err(|e| TgwError::WebView(e.to_string()))?;
        if let Some(bounds) = parent {
            webview
                .set_bounds(bounds)
                .map_err(|e| TgwError::WebView(e.to_string()))?;
        }
        webview
            .navigate_to_data(WIDGET_ENTRY_DOC_ID)
            .map_err(|e| TgwError::WebView(e.to_string()))?;

        Ok(Self { webview, policy })
    }

    /// Resizes the embedded WebView2 bounds.
    pub fn set_bounds(&self, bounds: WebViewBounds) -> Result<()> {
        self.webview
            .set_bounds(bounds)
            .map_err(|e| TgwError::WebView(e.to_string()))
    }

    /// Shows or hides the embedded WebView2.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        self.webview
            .set_visible(visible)
            .map_err(|e| TgwError::WebView(e.to_string()))
    }

    /// Evaluates a JSON host message inside the sandboxed page.
    pub fn post_host_json(&self, json: &str) -> Result<()> {
        let escaped = serde_json::to_string(json)
            .map_err(|e| TgwError::Bridge(format!("escape host JSON: {e}")))?;
        let script = format!(
            "window.dispatchEvent(new CustomEvent('tgw:host',{{detail:JSON.parse({escaped})}}));"
        );
        self.webview
            .eval(&script)
            .map_err(|e| TgwError::WebView(e.to_string()))
    }

    /// Returns the active [`SandboxPolicy`].
    #[must_use]
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Explicitly closes the WebView2 controller.
    pub fn close(&self) {
        self.webview.close();
    }
}

// =============================================================================
// Windows OS Sandboxing Enforcement (`windows-sys` with `// SAFETY:` notes)
// =============================================================================

/// Windows OS sandboxing primitives: Job objects, restricted Low-integrity tokens, and
/// protected-DACL named pipe channels (`widget_sandbox_win.cpp` & `widget_channel_win.cpp`).
#[cfg(windows)]
pub mod win {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, FALSE, GENERIC_READ, GENERIC_WRITE, GetLastError,
        HANDLE, INVALID_HANDLE_VALUE, TRUE,
    };
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, CreateRestrictedToken, CreateWellKnownSid,
        DISABLE_MAX_PRIVILEGE, EqualSid, GetLengthSid, GetTokenInformation, InitializeAcl,
        InitializeSecurityDescriptor, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        SID_AND_ATTRIBUTES, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
        TOKEN_GROUPS, TOKEN_MANDATORY_LABEL, TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_TYPE, TOKEN_USER,
        TokenGroups, TokenIntegrityLevel, TokenPrivileges, TokenRestrictedSids, TokenType,
        TokenUser, WinLowLabelSid, WinNullSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ALL_ACCESS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::SystemServices::{
        SE_GROUP_ENABLED, SE_GROUP_INTEGRITY, SE_GROUP_USE_FOR_DENY_ONLY,
        SECURITY_DESCRIPTOR_REVISION,
    };

    /// Maximum SID byte size from `winnt.h` (`sizeof(SID) - sizeof(DWORD) + 15 * sizeof(DWORD)`).
    const SECURITY_MAX_SID_SIZE: u32 = 68;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, GetCurrentProcess, OpenProcessToken,
    };

    use super::{CHANNEL_PIPE_BUFFER_BYTES, JobLimitsPolicy, SE_CHANGE_NOTIFY_LUID_LOW};
    use crate::error::{Result, TgwError};

    /// RAII wrapper around a Win32 `HANDLE` that closes it on drop (`Platform::WidgetHelper::UniqueHandle`).
    #[derive(Debug)]
    pub struct WinHandle(HANDLE);

    // SAFETY: Win32 kernel handles (Job objects, Tokens, Named Pipes) are process-wide kernel
    // object references safe to transfer between threads.
    unsafe impl Send for WinHandle {}
    // SAFETY: Read-only queries / closing via `&self` or `&mut self` are thread-safe kernel calls.
    unsafe impl Sync for WinHandle {}

    impl WinHandle {
        /// Wraps a raw Win32 `HANDLE`, returning `None` for 0 or `INVALID_HANDLE_VALUE`.
        #[must_use]
        pub fn from_raw(raw: HANDLE) -> Option<Self> {
            if raw == 0 || raw == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(Self(raw))
            }
        }

        /// Returns the underlying raw `HANDLE`.
        #[must_use]
        pub fn as_raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for WinHandle {
        fn drop(&mut self) {
            if self.0 != 0 && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: `self.0` is an owned valid kernel handle acquired from Win32 APIs and
                // closed exactly once here on Drop.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    /// Configured Windows Job object handle and creation flags (`Platform::WidgetHelper::Job`).
    #[derive(Debug)]
    pub struct SandboxJob {
        /// Owned job object handle.
        pub handle: WinHandle,
        /// Additional `CreateProcessAsUserW` flags (e.g. `CREATE_BREAKAWAY_FROM_JOB` if needed).
        pub creation_flags: u32,
    }

    fn last_err(call: &str) -> TgwError {
        // SAFETY: `GetLastError` reads thread-local Win32 error state with no preconditions.
        let code = unsafe { GetLastError() };
        TgwError::Sandbox(format!("{call} failed with Win32 error {code}"))
    }

    /// Creates and configures a sandbox Win32 Job object enforcing `policy`
    /// (`Platform::WidgetHelper::CreateJob` in `widget_sandbox_win.cpp:888-942`).
    pub fn create_sandbox_job(policy: &JobLimitsPolicy) -> Result<SandboxJob> {
        let mut in_job = 0i32;
        // SAFETY: `GetCurrentProcess()` returns the pseudo-handle for the current process, and
        // `in_job` is a valid mutable stack `BOOL` pointer.
        if unsafe { IsProcessInJob(GetCurrentProcess(), 0, &mut in_job) } == 0 {
            return Err(last_err("IsProcessInJob"));
        }

        let mut creation_flags = 0u32;
        if in_job != 0 {
            // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain C struct safe to zero-init.
            let mut own: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            // SAFETY: Passing `0` queries the current process's job object into our
            // properly sized stack struct `own`.
            if unsafe {
                QueryInformationJobObject(
                    0,
                    JobObjectExtendedLimitInformation,
                    (&mut own as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    null_mut(),
                )
            } != 0
            {
                let limits = own.BasicLimitInformation.LimitFlags;
                if (limits & JOB_OBJECT_LIMIT_BREAKAWAY_OK) != 0
                    && (limits & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) == 0
                {
                    creation_flags = CREATE_BREAKAWAY_FROM_JOB;
                }
            }
        }

        // SAFETY: Creating an anonymous job object with default security descriptor.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        let handle = WinHandle::from_raw(raw).ok_or_else(|| last_err("CreateJobObjectW"))?;

        // SAFETY: Plain C structs safe to zero-initialize before populating limit fields.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = policy.limit_flags;
        limits.BasicLimitInformation.ActiveProcessLimit = policy.active_process_limit;

        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: policy.ui_restrictions,
        };

        // SAFETY: `handle.as_raw()` is a valid Job object handle and both struct pointers match
        // their corresponding `JOBOBJECTINFOCLASS` and byte sizes.
        let ok_limits = unsafe {
            SetInformationJobObject(
                handle.as_raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok_limits == 0 {
            return Err(last_err("SetInformationJobObject(ExtendedLimit)"));
        }

        // SAFETY: Same valid Job object handle and matching UI restrictions struct size.
        let ok_ui = unsafe {
            SetInformationJobObject(
                handle.as_raw(),
                JobObjectBasicUIRestrictions,
                (&ui as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            )
        };
        if ok_ui == 0 {
            return Err(last_err("SetInformationJobObject(BasicUIRestrictions)"));
        }

        Ok(SandboxJob {
            handle,
            creation_flags,
        })
    }

    /// Queries a Win32 Job object and verifies its extended limits and UI restrictions match `policy`
    /// (`Platform::WidgetHelper::VerifyJob` in `widget_sandbox_win.cpp:972-991`).
    pub fn verify_job_limits(job: &WinHandle, policy: &JobLimitsPolicy) -> Result<()> {
        // SAFETY: Plain C structs safe to zero-initialize.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        let mut ui: JOBOBJECT_BASIC_UI_RESTRICTIONS = unsafe { zeroed() };

        // SAFETY: `job.as_raw()` is an owned valid Job handle; buffer sizes match struct sizes.
        if unsafe {
            QueryInformationJobObject(
                job.as_raw(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(last_err("QueryInformationJobObject(ExtendedLimit)"));
        }
        // SAFETY: Same valid Job handle with matching `JOBOBJECT_BASIC_UI_RESTRICTIONS` size.
        if unsafe {
            QueryInformationJobObject(
                job.as_raw(),
                JobObjectBasicUIRestrictions,
                (&mut ui as *mut JOBOBJECT_BASIC_UI_RESTRICTIONS).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(last_err("QueryInformationJobObject(BasicUIRestrictions)"));
        }

        let basic = limits.BasicLimitInformation;
        if (basic.LimitFlags & policy.limit_flags) != policy.limit_flags
            || basic.ActiveProcessLimit != policy.active_process_limit
        {
            return Err(TgwError::Sandbox(format!(
                "job limits mismatch: flags=0x{:x}, active={}",
                basic.LimitFlags, basic.ActiveProcessLimit
            )));
        }
        if (ui.UIRestrictionsClass & policy.ui_restrictions) != policy.ui_restrictions {
            return Err(TgwError::Sandbox(format!(
                "job UI restrictions mismatch: 0x{:x}",
                ui.UIRestrictionsClass
            )));
        }
        Ok(())
    }

    fn read_token_info(token: HANDLE, class: i32) -> Result<Vec<u8>> {
        let mut needed = 0u32;
        // SAFETY: First call queries required buffer size by passing `null_mut()` and length 0.
        unsafe {
            GetTokenInformation(token, class, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(last_err("GetTokenInformation(size)"));
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: `buf` has exact capacity `needed` returned by the size query above.
        if unsafe {
            GetTokenInformation(
                token,
                class,
                buf.as_mut_ptr().cast::<c_void>(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last_err("GetTokenInformation"));
        }
        buf.truncate(needed as usize);
        Ok(buf)
    }

    fn create_well_known_sid(sid_type: i32) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = buf.len() as u32;
        // SAFETY: `buf` is sized to `SECURITY_MAX_SID_SIZE` and `size` receives actual SID length.
        if unsafe {
            CreateWellKnownSid(
                sid_type,
                null_mut(),
                buf.as_mut_ptr().cast::<c_void>(),
                &mut size,
            )
        } == 0
        {
            return Err(last_err("CreateWellKnownSid"));
        }
        buf.truncate(size as usize);
        Ok(buf)
    }

    /// Creates a Low-integrity restricted lockdown token derived from the current process token
    /// with `DISABLE_MAX_PRIVILEGE`, all user/group SIDs marked deny-only, and restricting SID set
    /// strictly to `WinNullSid` (`S-1-0-0`) (`CreateLowRestricted` in `widget_sandbox_win.cpp:403-437`).
    pub fn create_restricted_lockdown_token() -> Result<WinHandle> {
        let mut source: HANDLE = 0;
        // SAFETY: Opening current process token with query/duplicate/assign/adjust-default access.
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                &mut source,
            )
        } == 0
        {
            return Err(last_err("OpenProcessToken"));
        }
        let source = WinHandle::from_raw(source).ok_or_else(|| last_err("OpenProcessToken"))?;

        let user_buf = read_token_info(source.as_raw(), TokenUser)?;
        let groups_buf = read_token_info(source.as_raw(), TokenGroups)?;
        let mut low_sid = create_well_known_sid(WinLowLabelSid)?;
        let mut null_sid = create_well_known_sid(WinNullSid)?;

        if user_buf.len() < size_of::<TOKEN_USER>() || groups_buf.len() < size_of::<TOKEN_GROUPS>()
        {
            return Err(TgwError::Sandbox(
                "truncated token user/groups buffer".to_owned(),
            ));
        }

        // SAFETY: `user_buf` was populated by `GetTokenInformation(TokenUser)` and length-checked.
        let user_sid = unsafe { (*user_buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut disable = vec![SID_AND_ATTRIBUTES {
            Sid: user_sid,
            Attributes: 0,
        }];

        // SAFETY: `groups_buf` was populated by `GetTokenInformation(TokenGroups)`; `GroupCount`
        // entries start at `groups.Groups` within `groups_buf`.
        unsafe {
            let groups = &*groups_buf.as_ptr().cast::<TOKEN_GROUPS>();
            let slice =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            for g in slice {
                if (g.Attributes & (SE_GROUP_INTEGRITY as u32)) == 0 {
                    disable.push(SID_AND_ATTRIBUTES {
                        Sid: g.Sid,
                        Attributes: 0,
                    });
                }
            }
        }

        let restricting = [SID_AND_ATTRIBUTES {
            Sid: null_sid.as_mut_ptr().cast::<c_void>(),
            Attributes: 0,
        }];

        let mut restricted_raw: HANDLE = 0;
        // SAFETY: `source`, `disable`, and `restricting` all contain valid SIDs backed by live
        // buffers (`user_buf`, `groups_buf`, `null_sid`) for the duration of the call.
        if unsafe {
            CreateRestrictedToken(
                source.as_raw(),
                DISABLE_MAX_PRIVILEGE,
                disable.len() as u32,
                disable.as_ptr(),
                0,
                null(),
                restricting.len() as u32,
                restricting.as_ptr(),
                &mut restricted_raw,
            )
        } == 0
        {
            return Err(last_err("CreateRestrictedToken"));
        }
        let restricted =
            WinHandle::from_raw(restricted_raw).ok_or_else(|| last_err("CreateRestrictedToken"))?;

        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: low_sid.as_mut_ptr().cast::<c_void>(),
                Attributes: SE_GROUP_INTEGRITY as u32,
            },
        };
        // SAFETY: `low_sid` is a valid SID created by `CreateWellKnownSid(WinLowLabelSid)`.
        let sid_len = unsafe { GetLengthSid(low_sid.as_mut_ptr().cast::<c_void>()) };
        let info_len = (size_of::<TOKEN_MANDATORY_LABEL>() as u32).saturating_add(sid_len);
        // SAFETY: Setting `TokenIntegrityLevel` on the freshly created restricted primary token.
        if unsafe {
            SetTokenInformation(
                restricted.as_raw(),
                TokenIntegrityLevel,
                (&label as *const TOKEN_MANDATORY_LABEL).cast::<c_void>(),
                info_len,
            )
        } == 0
        {
            return Err(last_err("SetTokenInformation(TokenIntegrityLevel)"));
        }

        Ok(restricted)
    }

    /// Verifies that `token` satisfies tdesktop's lockdown token invariants:
    /// primary token, Low integrity, deny-only user & non-integrity groups, restricting SID
    /// strictly `WinNullSid`, and no privileges other than `SeChangeNotifyPrivilege`
    /// (`CheckLockdownToken` in `widget_sandbox_win.cpp:833-841`).
    pub fn verify_lockdown_token(token: &WinHandle) -> Result<()> {
        let raw = token.as_raw();

        // 1. Primary token check (`TokenPrimary == 1`).
        let ty = read_token_info(raw, TokenType)?;
        if ty.len() < size_of::<TOKEN_TYPE>() {
            return Err(TgwError::Sandbox("truncated TokenType".to_owned()));
        }
        // SAFETY: `ty` has size >= `size_of::<TOKEN_TYPE>()` from `GetTokenInformation`.
        let token_type = unsafe { *ty.as_ptr().cast::<TOKEN_TYPE>() };
        if token_type != 1 {
            return Err(TgwError::Sandbox("token is not a primary token".to_owned()));
        }

        // 2. Low integrity check.
        let label_buf = read_token_info(raw, TokenIntegrityLevel)?;
        let mut low_sid = create_well_known_sid(WinLowLabelSid)?;
        if label_buf.len() < size_of::<TOKEN_MANDATORY_LABEL>() {
            return Err(TgwError::Sandbox(
                "truncated TokenIntegrityLevel".to_owned(),
            ));
        }
        // SAFETY: Both SID pointers are valid for the duration of `EqualSid`.
        let is_low = unsafe {
            let label = &*label_buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            EqualSid(label.Label.Sid, low_sid.as_mut_ptr().cast::<c_void>()) != 0
        };
        if !is_low {
            return Err(TgwError::Sandbox(
                "token integrity level is not Low".to_owned(),
            ));
        }

        // 3. Deny-only user & groups check.
        let deny_only = |attrs: u32| -> bool {
            (attrs & (SE_GROUP_USE_FOR_DENY_ONLY as u32)) != 0
                && (attrs & (SE_GROUP_ENABLED as u32)) == 0
        };
        let user_buf = read_token_info(raw, TokenUser)?;
        let groups_buf = read_token_info(raw, TokenGroups)?;
        // SAFETY: Buffers populated by `GetTokenInformation` and verified non-empty.
        unsafe {
            let user = &*user_buf.as_ptr().cast::<TOKEN_USER>();
            if !deny_only(user.User.Attributes) {
                return Err(TgwError::Sandbox("token user is not deny-only".to_owned()));
            }
            let groups = &*groups_buf.as_ptr().cast::<TOKEN_GROUPS>();
            let slice =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            for g in slice {
                if (g.Attributes & (SE_GROUP_INTEGRITY as u32)) == 0 && !deny_only(g.Attributes) {
                    return Err(TgwError::Sandbox(format!(
                        "token group (0x{:x}) is not deny-only",
                        g.Attributes
                    )));
                }
            }
        }

        // 4. Restricting SIDs strictly [S-1-0-0].
        let restricting_buf = read_token_info(raw, TokenRestrictedSids)?;
        let mut null_sid = create_well_known_sid(WinNullSid)?;
        // SAFETY: `restricting_buf` was populated by `GetTokenInformation(TokenRestrictedSids)`.
        let null_only = unsafe {
            let groups = &*restricting_buf.as_ptr().cast::<TOKEN_GROUPS>();
            let slice =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            slice.len() == 1 && EqualSid(slice[0].Sid, null_sid.as_mut_ptr().cast::<c_void>()) != 0
        };
        if !null_only {
            return Err(TgwError::Sandbox(
                "token restricting SIDs are not strictly S-1-0-0".to_owned(),
            ));
        }

        // 5. Privilege check (at most SeChangeNotifyPrivilege).
        let privs_buf = read_token_info(raw, TokenPrivileges)?;
        // SAFETY: `privs_buf` was populated by `GetTokenInformation(TokenPrivileges)`.
        unsafe {
            let list = &*privs_buf.as_ptr().cast::<TOKEN_PRIVILEGES>();
            let slice =
                std::slice::from_raw_parts(list.Privileges.as_ptr(), list.PrivilegeCount as usize);
            for p in slice {
                if p.Luid.LowPart != SE_CHANGE_NOTIFY_LUID_LOW || p.Luid.HighPart != 0 {
                    return Err(TgwError::Sandbox(format!(
                        "token retains disallowed privilege LUID {}",
                        p.Luid.LowPart
                    )));
                }
            }
        }

        Ok(())
    }

    /// Creates a connected pair `(server_pipe, client_pipe)` over a single-instance, remote-client-
    /// rejecting named pipe protected by an owner-only DACL
    /// (`CreateWinChannel` in `widget_channel_win.cpp:42-127, 201-229`).
    pub fn create_sandbox_pipe_pair(pipe_name: &str) -> Result<(WinHandle, WinHandle)> {
        let mut token_raw: HANDLE = 0;
        // SAFETY: Querying current process token to obtain the owner SID for the pipe DACL.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_raw) } == 0 {
            return Err(last_err("OpenProcessToken"));
        }
        let token = WinHandle::from_raw(token_raw).ok_or_else(|| last_err("OpenProcessToken"))?;
        let user_buf = read_token_info(token.as_raw(), TokenUser)?;
        // SAFETY: `user_buf` holds `TOKEN_USER` from `GetTokenInformation`.
        let user_sid = unsafe { (*user_buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };

        let acl_size = size_of::<ACL>() + 64 + (SECURITY_MAX_SID_SIZE as usize);
        let mut acl_buf = vec![0u8; acl_size];
        let acl_ptr = acl_buf.as_mut_ptr().cast::<ACL>();
        // SAFETY: `SECURITY_DESCRIPTOR` is a plain Win32 struct initialized via `InitializeSecurityDescriptor`.
        let mut sd: SECURITY_DESCRIPTOR = unsafe { zeroed() };

        // SAFETY: `acl_ptr` points to a zeroed buffer of `acl_size` bytes; `user_sid` is valid.
        unsafe {
            if InitializeAcl(acl_ptr, acl_size as u32, ACL_REVISION) == 0 {
                return Err(last_err("InitializeAcl"));
            }
            if AddAccessAllowedAce(acl_ptr, ACL_REVISION, FILE_ALL_ACCESS, user_sid) == 0 {
                return Err(last_err("AddAccessAllowedAce"));
            }
            if InitializeSecurityDescriptor(
                (&mut sd as *mut SECURITY_DESCRIPTOR).cast::<c_void>(),
                SECURITY_DESCRIPTOR_REVISION,
            ) == 0
            {
                return Err(last_err("InitializeSecurityDescriptor"));
            }
            if SetSecurityDescriptorDacl(
                (&mut sd as *mut SECURITY_DESCRIPTOR).cast::<c_void>(),
                TRUE,
                acl_ptr,
                FALSE,
            ) == 0
            {
                return Err(last_err("SetSecurityDescriptorDacl"));
            }
            if SetSecurityDescriptorControl(
                (&mut sd as *mut SECURITY_DESCRIPTOR).cast::<c_void>(),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ) == 0
            {
                return Err(last_err("SetSecurityDescriptorControl"));
            }
        }

        let attrs = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&mut sd as *mut SECURITY_DESCRIPTOR).cast::<c_void>(),
            bInheritHandle: FALSE,
        };

        let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide_name` is null-terminated UTF-16 and `attrs` references live `sd` / `acl_buf`.
        let server_raw = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                CHANNEL_PIPE_BUFFER_BYTES,
                CHANNEL_PIPE_BUFFER_BYTES,
                0,
                &attrs,
            )
        };
        let server = WinHandle::from_raw(server_raw).ok_or_else(|| last_err("CreateNamedPipeW"))?;

        // SAFETY: Opens the single client instance of our newly created pipe.
        let client_raw = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                0,
            )
        };
        let client = WinHandle::from_raw(client_raw).ok_or_else(|| last_err("CreateFileW"))?;

        // SAFETY: Client already connected synchronously via `CreateFileW`, so `ConnectNamedPipe`
        // completes with `ERROR_PIPE_CONNECTED`.
        let connected = unsafe { ConnectNamedPipe(server.as_raw(), null_mut()) };
        if connected == 0 {
            // SAFETY: Thread-local Win32 error read.
            let err = unsafe { GetLastError() };
            if err != ERROR_PIPE_CONNECTED {
                return Err(TgwError::Sandbox(format!(
                    "ConnectNamedPipe failed with Win32 error {err}"
                )));
            }
        }

        Ok((server, client))
    }
}

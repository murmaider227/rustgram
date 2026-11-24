//! Integration tests for all Telegram Desktop login flows against `FakeRpcClient`
//! (`tg_api::RpcClient` + real `tg-tl` wire serialization + real `tg-crypto::srp`).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{FakeRpcClient, SrpTestServer, rpc_err, sample_tl_auth, sample_tl_self_user};
use tg_api::error::{ApiError, LoginErrorKind, map_login_error};
use tg_api::login::{
    CallStatus, HANDLE_SRP_ID_INVALID_TIMEOUT, LoginStateMachine, LoginStep, PasswordCheckMode,
    encode_base64url, format_qr_login_url,
};
use tg_api::rpc::{RpcClient, RpcError};
use tg_api::types::ApiCredentials;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

fn test_creds() -> ApiCredentials {
    ApiCredentials {
        api_id: 99_999,
        api_hash: "test_api_hash_from_config".into(),
    }
}

fn empty_dialog_filters() -> tl_enums::messages::DialogFilters {
    tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
        tags_enabled: false,
        filters: Vec::new(),
    })
}

fn sent_code(
    code_type: tl_enums::auth::SentCodeType,
    phone_code_hash: &str,
    next_type: Option<tl_enums::auth::CodeType>,
    timeout: Option<i32>,
) -> tl_enums::auth::SentCode {
    tl_enums::auth::SentCode::SentCode(tl_types::auth::SentCode {
        r#type: code_type,
        phone_code_hash: phone_code_hash.to_owned(),
        next_type,
        timeout,
    })
}

#[tokio::test]
async fn login_phone_app_code_success_with_migrate_and_preloaded_filters() {
    let client = FakeRpcClient::new(2);
    client.enqueue_ok::<tl_fns::help::GetNearestDc>(&tl_enums::NearestDc::NearestDc(
        tl_types::NearestDc {
            country: "DE".into(),
            this_dc: 2,
            nearest_dc: 4,
        },
    ));
    // First sendCode triggers PHONE_MIGRATE_5, second succeeds.
    client.enqueue_err::<tl_fns::auth::SendCode>(rpc_err(303, "PHONE_MIGRATE_5"));
    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::App(tl_types::auth::SentCodeTypeApp { length: 5 }),
        "hash_app_1",
        Some(tl_enums::auth::CodeType::Sms),
        Some(120),
    ));
    client.enqueue_ok::<tl_fns::auth::SignIn>(&sample_tl_auth(sample_tl_self_user()));
    client.enqueue_ok::<tl_fns::messages::GetDialogFilters>(
        &tl_enums::messages::DialogFilters::DialogFilters(tl_types::messages::DialogFilters {
            tags_enabled: true,
            filters: Vec::new(),
        }),
    );

    let mut sm = LoginStateMachine::new(test_creds(), "US");
    let nearest = sm.fetch_nearest_dc(&client).await.unwrap();
    assert_eq!(nearest.nearest_dc, 4);
    assert_eq!(client.main_dc_id(), 4);

    // Reject single-digit phone locally.
    assert_eq!(
        sm.submit_phone(&client, "+1").await,
        Err(ApiError::Login(LoginErrorKind::PhoneNumberInvalid))
    );

    sm.enter_phone_input();
    let step = sm.submit_phone(&client, "+999 662 1111").await.unwrap();
    assert_eq!(client.main_dc_id(), 5);
    match step {
        LoginStep::CodeInput(code) => {
            assert_eq!(code.phone, "9996621111");
            assert_eq!(code.phone_hash, "hash_app_1");
            assert!(code.code_by_telegram);
            assert_eq!(code.code_length, 5);
            assert_eq!(code.call_status, CallStatus::Disabled);
        }
        other => panic!("expected CodeInput, got {other:?}"),
    }

    // Wrong length rejected locally before hitting server.
    assert_eq!(
        sm.submit_code(&client, "123").await,
        Err(ApiError::Login(LoginErrorKind::PhoneCodeInvalid))
    );

    let step = sm.submit_code(&client, "22222").await.unwrap();
    match step {
        LoginStep::Authorized {
            user,
            preloaded_filters,
            password_recovered,
            ..
        } => {
            assert_eq!(user.id, 123_456);
            assert!(preloaded_filters.tags_enabled);
            assert!(!password_recovered);
        }
        other => panic!("expected Authorized, got {other:?}"),
    }

    // Verify sendCode passed allow_flashcall = false and credentials from config.
    let send_calls = client.calls_of::<tl_fns::auth::SendCode>();
    assert_eq!(send_calls.len(), 2);
    assert_eq!(send_calls[0].0, 4);
    assert_eq!(send_calls[1].0, 5);
    let tl_enums::CodeSettings::CodeSettings(settings) = &send_calls[0].1.settings;
    assert!(!settings.allow_flashcall);
    assert!(!settings.allow_firebase);
    assert_eq!(send_calls[0].1.api_id, 99_999);
    assert_eq!(send_calls[0].1.api_hash, "test_api_hash_from_config");
}

#[tokio::test]
async fn login_2fa_real_srp6a_verification_wrong_password_and_srp_id_invalid_retry() {
    let client = FakeRpcClient::new(2);
    let srp_v1 = Arc::new(SrpTestServer::new("correct_horse_battery_staple", 100));
    let srp_v2 = Arc::new(SrpTestServer::new("correct_horse_battery_staple", 101));
    let srp_v3 = Arc::new(SrpTestServer::new("correct_horse_battery_staple", 102));

    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms { length: 5 }),
        "hash_2fa",
        Some(tl_enums::auth::CodeType::Call),
        Some(60),
    ));
    client.enqueue_err::<tl_fns::auth::SignIn>(rpc_err(401, "SESSION_PASSWORD_NEEDED"));
    // Initial getPassword -> srp_id = 100.
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp_v1.tl_account_password(Some("my hint"), true),
    ));
    // Refresh after PASSWORD_HASH_INVALID -> srp_id = 101.
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp_v2.tl_account_password(Some("my hint"), true),
    ));
    // Refresh after SRP_ID_INVALID -> srp_id = 102.
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp_v3.tl_account_password(Some("my hint"), true),
    ));
    client.enqueue_ok::<tl_fns::messages::GetDialogFilters>(&empty_dialog_filters());

    // Dynamic server-side SRP-6a handler:
    // - Call 1 (srp_id 100): verifies wrong password against srp_v1 -> PASSWORD_HASH_INVALID
    // - Call 2 (srp_id 101): verifies valid proof against srp_v2, then simulates SRP_ID_INVALID
    // - Call 3 (srp_id 102): verifies auto-retried proof against srp_v3 -> Authorized!
    let attempt = Arc::new(Mutex::new(0u32));
    let s1 = Arc::clone(&srp_v1);
    let s2 = Arc::clone(&srp_v2);
    let s3 = Arc::clone(&srp_v3);
    let attempt_cl = Arc::clone(&attempt);
    client.set_handler::<tl_fns::auth::CheckPassword, _>(move |_dc, req| {
        let mut n = attempt_cl.lock().unwrap();
        *n += 1;
        let check = match req.password {
            tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(c) => c,
            tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty => {
                return Err(rpc_err(400, "PASSWORD_EMPTY"));
            }
        };
        match *n {
            1 => {
                assert_eq!(check.srp_id, 100);
                if s1.verify_check(&check) {
                    Ok(sample_tl_auth(sample_tl_self_user()))
                } else {
                    Err(rpc_err(400, "PASSWORD_HASH_INVALID"))
                }
            }
            2 => {
                assert_eq!(check.srp_id, 101);
                // Client computed valid SRP-6a M1 for srp_v2; simulate expired srp_id.
                assert!(s2.verify_check(&check));
                Err(rpc_err(400, "SRP_ID_INVALID"))
            }
            3 => {
                assert_eq!(check.srp_id, 102);
                assert!(
                    s3.verify_check(&check),
                    "server-side SRP-6a verification of client M1 must succeed!"
                );
                Ok(sample_tl_auth(sample_tl_self_user()))
            }
            other => panic!("unexpected checkPassword attempt {other}"),
        }
    });

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996621111").await.unwrap();
    let step = sm.submit_code(&client, "22222").await.unwrap();
    match step {
        LoginStep::PasswordCheck {
            password_state,
            mode,
            from_qr,
            ..
        } => {
            assert_eq!(password_state.hint, "my hint");
            assert!(password_state.has_recovery);
            assert_eq!(*mode, PasswordCheckMode::Password);
            assert!(!from_qr);
        }
        other => panic!("expected PasswordCheck, got {other:?}"),
    }

    let now = Instant::now();
    // 1. Wrong password -> server rejects M1 -> PasswordInvalid.
    let err = sm
        .submit_password(&client, "wrong_password", now)
        .await
        .unwrap_err();
    assert_eq!(err, ApiError::Login(LoginErrorKind::PasswordInvalid));

    // 2. Correct password -> first attempt hits SRP_ID_INVALID, auto-refreshes to srp_id=101,
    //    recomputes SRP-6a with cached password hash, server verifies M1 -> Authorized!
    let step = sm
        .submit_password(&client, "correct_horse_battery_staple", now)
        .await
        .unwrap();
    assert!(matches!(step, LoginStep::Authorized { .. }));

    let check_calls = client.calls_of::<tl_fns::auth::CheckPassword>();
    assert_eq!(check_calls.len(), 3);
}

#[tokio::test]
async fn login_2fa_repeated_srp_id_invalid_within_60s_fails() {
    let client = FakeRpcClient::new(2);
    let srp1 = SrpTestServer::new("pw", 10);
    let srp2 = SrpTestServer::new("pw", 11);

    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms { length: 5 }),
        "h",
        None,
        None,
    ));
    client.enqueue_err::<tl_fns::auth::SignIn>(rpc_err(401, "SESSION_PASSWORD_NEEDED"));
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp1.tl_account_password(None, false),
    ));
    client.enqueue_err::<tl_fns::auth::CheckPassword>(rpc_err(400, "SRP_ID_INVALID"));
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp2.tl_account_password(None, false),
    ));
    client.enqueue_err::<tl_fns::auth::CheckPassword>(rpc_err(400, "SRP_ID_INVALID"));

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996621111").await.unwrap();
    sm.submit_code(&client, "22222").await.unwrap();

    let t0 = Instant::now();
    let err = sm.submit_password(&client, "pw", t0).await.unwrap_err();
    assert!(matches!(err, ApiError::ServerError(_)));
    let _ = HANDLE_SRP_ID_INVALID_TIMEOUT;
}

#[tokio::test]
async fn login_password_empty_go_back_restores_code_input_when_from_code() {
    let client = FakeRpcClient::new(2);
    let srp = SrpTestServer::new("pw", 77);

    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::App(tl_types::auth::SentCodeTypeApp { length: 5 }),
        "hash_code_back",
        None,
        None,
    ));
    client.enqueue_err::<tl_fns::auth::SignIn>(rpc_err(401, "SESSION_PASSWORD_NEEDED"));
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp.tl_account_password(Some("hint"), false),
    ));
    client.enqueue_err::<tl_fns::auth::CheckPassword>(rpc_err(400, "PASSWORD_EMPTY"));

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996621111").await.unwrap();
    sm.submit_code(&client, "22222").await.unwrap();
    assert!(matches!(
        sm.step(),
        LoginStep::PasswordCheck { from_qr: false, .. }
    ));

    // PASSWORD_EMPTY triggers go_back() -> restores CodeInput with exact phone & hash!
    let err = sm
        .submit_password(&client, "pw", Instant::now())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ApiError::Login(LoginErrorKind::PasswordEmptyOrUnregistered)
    );
    match sm.step() {
        LoginStep::CodeInput(c) => {
            assert_eq!(c.phone, "9996621111");
            assert_eq!(c.phone_hash, "hash_code_back");
            assert_eq!(c.code_length, 5);
        }
        other => panic!("expected CodeInput after go_back from code-origin 2FA, got {other:?}"),
    }

    // Explicit go_back() from CodeInput returns to PhoneInput.
    assert!(sm.go_back());
    assert_eq!(sm.step(), &LoginStep::PhoneInput);
}

#[tokio::test]
async fn login_password_empty_go_back_restores_qr_login_when_from_qr() {
    let client = FakeRpcClient::new(2);
    let srp = SrpTestServer::new("pw", 88);

    client.enqueue_ok::<tl_fns::auth::ExportLoginToken>(&tl_enums::auth::LoginToken::LoginToken(
        tl_types::auth::LoginToken {
            expires: 1_700_000_030,
            token: vec![0x10, 0x20, 0x30],
        },
    ));
    // Refresh after updateLoginToken returns SESSION_PASSWORD_NEEDED.
    client.enqueue_err::<tl_fns::auth::ExportLoginToken>(rpc_err(401, "SESSION_PASSWORD_NEEDED"));
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp.tl_account_password(None, false),
    ));
    client.enqueue_err::<tl_fns::auth::CheckPassword>(rpc_err(401, "AUTH_KEY_UNREGISTERED"));

    let mut sm = LoginStateMachine::new(test_creds(), "US");
    sm.start_qr_login(&client, 1_700_000_000, Vec::new())
        .await
        .unwrap();
    assert!(matches!(sm.step(), LoginStep::QrLogin(_)));

    // Simulate updateLoginToken triggering refresh -> enters PasswordCheck with from_qr = true.
    let handled = sm
        .on_qr_updates(
            &client,
            &tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::LoginToken,
                date: 1_700_000_005,
            }),
            1_700_000_005,
        )
        .await
        .unwrap();
    assert!(handled);
    assert!(matches!(
        sm.step(),
        LoginStep::PasswordCheck { from_qr: true, .. }
    ));

    // AUTH_KEY_UNREGISTERED triggers go_back() -> restores QrLogin with original QR state!
    let err = sm
        .submit_password(&client, "pw", Instant::now())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ApiError::Login(LoginErrorKind::PasswordEmptyOrUnregistered)
    );
    match sm.step() {
        LoginStep::QrLogin(qr) => {
            assert_eq!(qr.token, vec![0x10, 0x20, 0x30]);
        }
        other => panic!("expected QrLogin after go_back from QR-origin 2FA, got {other:?}"),
    }
}

#[tokio::test]
async fn login_2fa_recovery_via_email_and_account_reset() {
    let client = FakeRpcClient::new(2);
    let srp = SrpTestServer::new("pw", 50);

    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms { length: 5 }),
        "h_rec",
        None,
        None,
    ));
    client.enqueue_err::<tl_fns::auth::SignIn>(rpc_err(401, "SESSION_PASSWORD_NEEDED"));
    client.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp.tl_account_password(None, true),
    ));
    client.enqueue_ok::<tl_fns::auth::RequestPasswordRecovery>(
        &tl_enums::auth::PasswordRecovery::PasswordRecovery(tl_types::auth::PasswordRecovery {
            email_pattern: "t***@example.org".into(),
        }),
    );
    client.enqueue_err::<tl_fns::auth::CheckRecoveryPassword>(rpc_err(400, "CODE_INVALID"));
    client.enqueue_ok::<tl_fns::auth::CheckRecoveryPassword>(&true);
    client.enqueue_ok::<tl_fns::auth::RecoverPassword>(&sample_tl_auth(sample_tl_self_user()));
    client.enqueue_ok::<tl_fns::messages::GetDialogFilters>(&empty_dialog_filters());

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996621111").await.unwrap();
    sm.submit_code(&client, "12345").await.unwrap();

    let pattern = sm.request_password_recovery(&client).await.unwrap();
    assert_eq!(pattern, "t***@example.org");

    assert_eq!(
        sm.check_recovery_code(&client, "000000").await,
        Err(ApiError::Login(LoginErrorKind::RecoveryCodeInvalid))
    );
    sm.check_recovery_code(&client, " 654321 ").await.unwrap();
    let step = sm.complete_password_recovery(&client).await.unwrap();
    assert!(matches!(
        step,
        LoginStep::Authorized {
            password_recovered: true,
            ..
        }
    ));

    // Also test reset_account path.
    let client2 = FakeRpcClient::new(2);
    client2.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms { length: 5 }),
        "h_rst",
        None,
        None,
    ));
    client2.enqueue_err::<tl_fns::auth::SignIn>(rpc_err(401, "SESSION_PASSWORD_NEEDED"));
    client2.enqueue_ok::<tl_fns::account::GetPassword>(&tl_enums::account::Password::Password(
        srp.tl_account_password(None, false),
    ));
    client2.enqueue_ok::<tl_fns::account::DeleteAccount>(&true);

    let mut sm2 = LoginStateMachine::at_phone_input(test_creds());
    sm2.submit_phone(&client2, "+9996621111").await.unwrap();
    sm2.submit_code(&client2, "12345").await.unwrap();
    let step = sm2.reset_account(&client2).await.unwrap();
    assert!(matches!(step, LoginStep::SignUp { .. }));
    let del_calls = client2.calls_of::<tl_fns::account::DeleteAccount>();
    assert_eq!(del_calls.len(), 1);
    assert_eq!(del_calls[0].1.reason, "Forgot password");
}

#[tokio::test]
async fn login_signup_with_terms_of_service_popup() {
    let client = FakeRpcClient::new(2);
    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms { length: 5 }),
        "h_new",
        None,
        None,
    ));
    client.enqueue_ok::<tl_fns::auth::SignIn>(&tl_enums::auth::Authorization::SignUpRequired(
        tl_types::auth::AuthorizationSignUpRequired {
            terms_of_service: Some(tl_enums::help::TermsOfService::TermsOfService(
                tl_types::help::TermsOfService {
                    popup: true,
                    id: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                        data: "{\"data\":\"tos1\"}".into(),
                    }),
                    text: "Accept Terms of Service".into(),
                    entities: Vec::new(),
                    min_age_confirm: Some(16),
                },
            )),
        },
    ));
    client.enqueue_ok::<tl_fns::auth::SignUp>(&sample_tl_auth(sample_tl_self_user()));
    client.enqueue_ok::<tl_fns::messages::GetDialogFilters>(&empty_dialog_filters());

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996629999").await.unwrap();
    let step = sm.submit_code(&client, "22222").await.unwrap();
    assert!(matches!(step, LoginStep::SignUp { .. }));

    // Attempting sign up before accepting popup ToS fails with TermsNotAccepted.
    assert_eq!(
        sm.submit_sign_up(&client, "Alpha", "Tester").await,
        Err(ApiError::Login(LoginErrorKind::TermsNotAccepted))
    );

    sm.accept_terms().unwrap();
    let step = sm
        .submit_sign_up(&client, " Alpha ", " Tester ")
        .await
        .unwrap();
    assert!(matches!(step, LoginStep::Authorized { .. }));
    let su = client.calls_of::<tl_fns::auth::SignUp>();
    assert_eq!(su[0].1.first_name, "Alpha");
    assert_eq!(su[0].1.last_name, "Tester");
}

#[tokio::test]
async fn login_qr_with_refresh_update_and_dc_migration() {
    let client = FakeRpcClient::new(2);
    client.enqueue_ok::<tl_fns::auth::ExportLoginToken>(&tl_enums::auth::LoginToken::LoginToken(
        tl_types::auth::LoginToken {
            expires: 1_700_000_030,
            token: vec![0x01, 0x02, 0xFF, 0xFE],
        },
    ));
    // After UpdateLoginToken arrives, exportLoginToken returns MigrateTo DC 4.
    client.enqueue_ok::<tl_fns::auth::ExportLoginToken>(&tl_enums::auth::LoginToken::MigrateTo(
        tl_types::auth::LoginTokenMigrateTo {
            dc_id: 4,
            token: vec![0xDE, 0xAD, 0xBE, 0xEF],
        },
    ));
    // importLoginToken on DC 4 returns Success.
    client.enqueue_ok::<tl_fns::auth::ImportLoginToken>(&tl_enums::auth::LoginToken::Success(
        tl_types::auth::LoginTokenSuccess {
            authorization: sample_tl_auth(sample_tl_self_user()),
        },
    ));
    client.enqueue_ok::<tl_fns::messages::GetDialogFilters>(&empty_dialog_filters());

    assert_eq!(encode_base64url(&[0x01, 0x02, 0xFF, 0xFE]), "AQL__g==");
    assert_eq!(
        format_qr_login_url(&[0x01, 0x02, 0xFF, 0xFE]),
        "tg://login?token=AQL__g=="
    );

    let mut sm = LoginStateMachine::new(test_creds(), "US");
    let step = sm
        .start_qr_login(&client, 1_700_000_000, Vec::new())
        .await
        .unwrap();
    match step {
        LoginStep::QrLogin(qr) => {
            assert_eq!(qr.qr_url, "tg://login?token=AQL__g==");
            assert_eq!(qr.refresh_after_secs, 30);
        }
        other => panic!("expected QrLogin, got {other:?}"),
    }

    // Receive updateLoginToken -> triggers refresh -> MigrateTo DC 4 -> importLoginToken -> Authorized.
    let handled = sm
        .on_qr_updates(
            &client,
            &tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update: tl_enums::Update::LoginToken,
                date: 1_700_000_005,
            }),
            1_700_000_005,
        )
        .await
        .unwrap();
    assert!(handled);
    assert_eq!(client.main_dc_id(), 4);
    assert!(matches!(sm.step(), LoginStep::Authorized { .. }));

    // Confirm importLoginToken was called on DC 4 with the migrated token.
    let imports = client.calls_of::<tl_fns::auth::ImportLoginToken>();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].0, 4);
    assert_eq!(imports[0].1.token, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[tokio::test]
async fn login_code_types_call_resend_timeout_and_email_setup() {
    let client = FakeRpcClient::new(2);
    // 1. SetUpEmailRequired -> EmailSetup -> EmailVerify -> CodeInput (FragmentSms)
    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::SetUpEmailRequired(
            tl_types::auth::SentCodeTypeSetUpEmailRequired {
                apple_signin_allowed: false,
                google_signin_allowed: false,
            },
        ),
        "h_email",
        None,
        None,
    ));
    client.enqueue_ok::<tl_fns::account::SendVerifyEmailCode>(
        &tl_enums::account::SentEmailCode::SentEmailCode(tl_types::account::SentEmailCode {
            email_pattern: "a***@example.org".into(),
            length: 6,
        }),
    );
    client.enqueue_ok::<tl_fns::account::VerifyEmail>(&tl_enums::account::EmailVerified::Login(
        tl_types::account::EmailVerifiedLogin {
            email: "alpha@example.org".into(),
            sent_code: sent_code(
                tl_enums::auth::SentCodeType::FragmentSms(
                    tl_types::auth::SentCodeTypeFragmentSms {
                        url: "https://fragment.example/code/xyz".into(),
                        length: 5,
                    },
                ),
                "h_frag",
                Some(tl_enums::auth::CodeType::Call),
                Some(2),
            ),
        },
    ));
    client.enqueue_ok::<tl_fns::auth::ResendCode>(&sent_code(
        tl_enums::auth::SentCodeType::Call(tl_types::auth::SentCodeTypeCall { length: 5 }),
        "h_frag",
        None,
        None,
    ));
    client.enqueue_ok::<tl_fns::auth::CancelCode>(&true);

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996621111").await.unwrap();
    assert!(matches!(sm.step(), LoginStep::EmailSetup { .. }));

    sm.submit_email_setup(&client, "alpha@example.org")
        .await
        .unwrap();
    assert!(matches!(sm.step(), LoginStep::EmailVerify { .. }));

    let step = sm.submit_code(&client, "123456").await.unwrap();
    match step {
        LoginStep::CodeInput(c) => {
            assert_eq!(
                c.fragment_url.as_deref(),
                Some("https://fragment.example/code/xyz")
            );
            assert_eq!(c.call_status, CallStatus::Waiting);
            assert_eq!(c.call_timeout_secs, 2);
        }
        other => panic!("expected CodeInput, got {other:?}"),
    }

    // Countdown: 2 -> 1 (still Waiting), 1 -> 0 (auto resendCode -> Called).
    assert_eq!(
        sm.tick_call_countdown(&client).await.unwrap(),
        CallStatus::Waiting
    );
    assert_eq!(
        sm.tick_call_countdown(&client).await.unwrap(),
        CallStatus::Called
    );

    sm.cancel_code(&client).await.unwrap();
    assert_eq!(sm.step(), &LoginStep::PhoneInput);
}

#[tokio::test]
async fn login_email_code_signin_and_error_mappings() {
    let client = FakeRpcClient::new(2);
    client.enqueue_ok::<tl_fns::auth::SendCode>(&sent_code(
        tl_enums::auth::SentCodeType::EmailCode(tl_types::auth::SentCodeTypeEmailCode {
            apple_signin_allowed: false,
            google_signin_allowed: false,
            email_pattern: "u***@example.org".into(),
            length: 6,
            reset_available_period: None,
            reset_pending_date: None,
        }),
        "h_em",
        None,
        None,
    ));
    client.enqueue_err::<tl_fns::auth::SignIn>(rpc_err(400, "PHONE_CODE_EXPIRED"));
    client.enqueue_ok::<tl_fns::auth::LogOut>(&tl_enums::auth::LoggedOut::LoggedOut(
        tl_types::auth::LoggedOut {
            future_auth_token: None,
        },
    ));

    let mut sm = LoginStateMachine::at_phone_input(test_creds());
    sm.submit_phone(&client, "+9996621111").await.unwrap();
    let err = sm.submit_code(&client, "654321").await.unwrap_err();
    assert_eq!(err, ApiError::Login(LoginErrorKind::PhoneCodeExpired));
    assert_eq!(sm.step(), &LoginStep::PhoneInput);

    // Check SignIn sent EmailVerification::Code variant.
    let sign_ins = client.calls_of::<tl_fns::auth::SignIn>();
    assert_eq!(sign_ins.len(), 1);
    assert_eq!(
        sign_ins[0].1.email_verification,
        Some(tl_enums::EmailVerification::Code(
            tl_types::EmailVerificationCode {
                code: "654321".into(),
            }
        ))
    );

    // Verify error text mappings.
    assert_eq!(
        map_login_error(&RpcError::new(400, "PHONE_NUMBER_INVALID")),
        LoginErrorKind::PhoneNumberInvalid
    );
    assert_eq!(
        map_login_error(&RpcError::new(400, "PHONE_CODE_INVALID")),
        LoginErrorKind::PhoneCodeInvalid
    );
    assert_eq!(
        map_login_error(&RpcError::new(420, "FLOOD_WAIT_42")),
        LoginErrorKind::FloodWait { seconds: 42 }
    );
    assert_eq!(
        map_login_error(&RpcError::new(420, "FLOOD_PREMIUM_WAIT_10")),
        LoginErrorKind::FloodWait { seconds: 10 }
    );
    assert_eq!(
        map_login_error(&RpcError::new(400, "2FA_CONFIRM_WAIT_3600")),
        LoginErrorKind::TwoFaConfirmWait { seconds: 3600 }
    );
    assert_eq!(
        map_login_error(&RpcError::new(406, "UPDATE_APP_TO_LOGIN")),
        LoginErrorKind::Ignored {
            raw: "UPDATE_APP_TO_LOGIN".into()
        }
    );

    let logged_out = sm.log_out(&client).await.unwrap();
    assert!(logged_out.future_auth_token.is_none());
    let _ = Duration::from_secs(1);
}

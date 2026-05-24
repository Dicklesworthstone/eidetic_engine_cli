#![forbid(unsafe_code)]

use ee::policy::redact_secret_like_content;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn memory_body_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            any::<char>().prop_map(|c| c.to_string()),
            Just("ordinary procedural memory body ".to_string()),
            Just("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string()),
            Just("password=super-secret-value-1234567890".to_string()),
            Just("sk-ant-api03-redactionfuzztokenredactionfuzztokenredactionfuzz".to_string()),
            Just("https://user:password@example.com/path".to_string()),
            Just(
                [
                    "-----BEGIN RSA PRIVATE KEY-----",
                    "MIIEowIBAAKCAQEAredactionidempotenceredactionidempotence",
                    "-----END RSA PRIVATE KEY-----",
                ]
                .join("\n"),
            ),
        ],
        0..128,
    )
    .prop_map(|pieces| pieces.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn redaction_is_idempotent_for_arbitrary_memory_bodies(body in memory_body_strategy()) {
        let once = redact_secret_like_content(&body);
        let twice = redact_secret_like_content(&once.content);

        prop_assert_eq!(
            &once.content,
            &twice.content,
            "applying redaction twice must produce byte-identical content",
        );
    }
}

#[test]
fn redaction_idempotence_covers_multiple_secret_shapes() {
    let body = [
        "Store this memory only after redaction.",
        "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "password=super-secret-value-1234567890",
        "sk-ant-api03-redactionfuzztokenredactionfuzztokenredactionfuzz",
        "https://user:password@example.com/path",
    ]
    .join("\n");

    let once = redact_secret_like_content(&body);
    let twice = redact_secret_like_content(&once.content);

    assert_eq!(
        once.content, twice.content,
        "redacted memory bodies must remain stable on subsequent redaction passes",
    );
}

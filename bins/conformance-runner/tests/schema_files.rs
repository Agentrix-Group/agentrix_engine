#[test]
fn every_versioned_schema_is_valid_json() {
    let schemas: &[(&str, &[u8])] = &[
        ("v2/action-batch", include_bytes!("../../../schemas/v2/action-batch.schema.json")),
        ("v2/execution-spec", include_bytes!("../../../schemas/v2/execution-spec.schema.json")),
        ("v2/frame", include_bytes!("../../../schemas/v2/frame.schema.json")),
        ("v2/game-descriptor", include_bytes!("../../../schemas/v2/game-descriptor.schema.json")),
        ("conformance/action", include_bytes!("../../../schemas/games/conformance-counter/1.0.0/action.schema.json")),
        ("conformance/config", include_bytes!("../../../schemas/games/conformance-counter/1.0.0/config.schema.json")),
        ("conformance/observation", include_bytes!("../../../schemas/games/conformance-counter/1.0.0/observation.schema.json")),
        ("conformance/public", include_bytes!("../../../schemas/games/conformance-counter/1.0.0/public.schema.json")),
        ("starfighter/action", include_bytes!("../../../schemas/games/starfighter/0.3.0-core.1/action.schema.json")),
        ("starfighter/observation", include_bytes!("../../../schemas/games/starfighter/0.3.0-core.1/observation.schema.json")),
        ("starfighter/public", include_bytes!("../../../schemas/games/starfighter/0.3.0-core.1/public.schema.json")),
    ];
    for (name, bytes) in schemas {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .unwrap_or_else(|error| {
                panic!("schema {name} is not JSON: {error}")
            });
        assert_eq!(
            value["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
    }
}

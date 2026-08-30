#[path = "../../firmware/build-checks.rs"]
mod build_checks;

const NETWORK: &str = include_str!("../../firmware/src/network.rs");
const CSS: &[u8] = include_bytes!("../../firmware/src/dashboard.css");
const JAVASCRIPT: &[u8] = include_bytes!("../../firmware/src/dashboard.js");

#[test]
fn production_dashboard_obeys_the_firmware_heap_contract() {
    build_checks::validate_dashboard(NETWORK, CSS, JAVASCRIPT).unwrap();
}

#[test]
fn rejects_styles_embedded_in_each_dynamic_response() {
    let unsafe_source = NETWORK.replace(
        "href=/dashboard.css",
        "href=/dashboard.css html.push_str(DASHBOARD_STYLE)",
    );
    let error = build_checks::validate_dashboard(&unsafe_source, CSS, JAVASCRIPT).unwrap_err();
    assert!(error.contains("constrained runtime heap"));
}

#[test]
fn rejects_scripts_embedded_in_each_dynamic_response() {
    let unsafe_source = NETWORK.replace(
        "src=/dashboard.js",
        "src=/dashboard.js html.push_str(DASHBOARD_SCRIPT)",
    );
    let error = build_checks::validate_dashboard(&unsafe_source, CSS, JAVASCRIPT).unwrap_err();
    assert!(error.contains("constrained runtime heap"));
}

#[test]
fn rejects_an_unbounded_dynamic_dashboard() {
    let unsafe_source = NETWORK.replace("DASHBOARD_BODY_CAPACITY", "UNBOUNDED_DASHBOARD_BODY");
    let error = build_checks::validate_dashboard(&unsafe_source, CSS, JAVASCRIPT).unwrap_err();
    assert!(error.contains("DASHBOARD_BODY_CAPACITY"));
}

#[test]
fn rejects_removing_response_serialization() {
    let unsafe_source = NETWORK.replace(
        "DASHBOARD_RESPONSE.lock().await",
        "unserialized_dashboard_response()",
    );
    let error = build_checks::validate_dashboard(&unsafe_source, CSS, JAVASCRIPT).unwrap_err();
    assert!(error.contains("DASHBOARD_RESPONSE.lock().await"));
}

#[test]
fn rejects_assets_that_outgrow_the_flash_response_budget() {
    let oversized_css = vec![0_u8; 16 * 1024 + 1];
    let error = build_checks::validate_dashboard(NETWORK, &oversized_css, JAVASCRIPT).unwrap_err();
    assert!(error.contains("dashboard.css"));
}

#[test]
fn rejects_removing_the_client_side_snapshot_clock() {
    const NEEDLE: &[u8] = b"setInterval(updateSnapshotAge, 1000)";
    let unsafe_javascript = JAVASCRIPT
        .windows(NEEDLE.len())
        .enumerate()
        .find(|(_, window)| *window == NEEDLE)
        .map(|(index, _)| {
            let mut javascript = JAVASCRIPT.to_vec();
            javascript.splice(
                index..index + NEEDLE.len(),
                b"/* freshness clock removed */".iter().copied(),
            );
            javascript
        })
        .expect("production JavaScript has the freshness clock");
    let error = build_checks::validate_dashboard(NETWORK, CSS, &unsafe_javascript).unwrap_err();
    assert!(error.contains("freshness invariant"));
}

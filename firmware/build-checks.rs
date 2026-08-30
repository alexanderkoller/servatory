const MAX_DASHBOARD_CSS_BYTES: usize = 16 * 1024;
const MAX_DASHBOARD_JS_BYTES: usize = 4 * 1024;

pub fn validate_dashboard(network: &str, css: &[u8], javascript: &[u8]) -> Result<(), String> {
    check_limit("dashboard.css", css.len(), MAX_DASHBOARD_CSS_BYTES)?;
    check_limit("dashboard.js", javascript.len(), MAX_DASHBOARD_JS_BYTES)?;

    for forbidden in [
        "concat!(\"<style>\", include_str!(\"dashboard.css\")",
        "concat!(\"<script>\", include_str!(\"dashboard.js\")",
        "html.push_str(DASHBOARD_STYLE)",
        "html.push_str(DASHBOARD_SCRIPT)",
    ] {
        if network.contains(forbidden) {
            return Err(format!(
                "firmware dashboard assets must be served as static responses; `{forbidden}` would charge them to the constrained runtime heap"
            ));
        }
    }

    for required in [
        "href=/dashboard.css",
        "src=/dashboard.js",
        "DASHBOARD_BODY_CAPACITY",
        "DASHBOARD_RESPONSE.lock().await",
    ] {
        if !network.contains(required) {
            return Err(format!(
                "firmware dashboard memory invariant is missing `{required}`"
            ));
        }
    }
    Ok(())
}

fn check_limit(name: &str, actual: usize, maximum: usize) -> Result<(), String> {
    if actual > maximum {
        return Err(format!(
            "{name} is {actual} bytes; firmware build limit is {maximum} bytes"
        ));
    }
    Ok(())
}
